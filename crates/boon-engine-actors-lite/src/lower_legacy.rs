use crate::bridge::{
    HostButtonLabel, HostCrossAlign, HostElementEventBinding, HostSelectOption,
    HostStripeDirection, HostTemplatedTextPart, HostViewIr, HostViewKind, HostViewMatchArm,
    HostViewMatchValue, HostViewNode, HostWidth,
};
use crate::cells_lower::{LoweredCellsFormula, parse_lowered_cells_formula};
use crate::cells_preview::CellsProgram;
use crate::cells_runtime::{CellsFormulaState, CellsSheetState};
use crate::host_view_template::{
    HostViewTemplate, HostViewTemplateCondition, HostViewTemplateNode, HostViewTemplateNodeKind,
    HostViewTemplateValue, materialize_host_view_template,
};
use crate::ir::{
    FunctionInstanceId, IrNode, IrNodeKind, IrNodePersistence, IrProgram, MirrorCellId, NodeId,
    PersistKind, RetainedNodeKey, SinkPortId, SourcePortId, ViewSiteId,
};
use crate::ir_executor::IrExecutor;
use crate::parse::{
    StaticExpression, StaticSpannedExpression, binding_at_path, contains_hold_expression,
    contains_latest_expression, contains_then_expression, contains_when_expression,
    contains_while_expression, parse_static_expressions, persist_entry_for_path,
    require_alias_paths, require_binding_at_path, require_function_call_paths,
    require_hold_binding_at_path, require_text_fragments, require_top_level_bindings,
    require_top_level_functions, top_level_bindings,
};
use boon::parser::static_expression::{Comparator, Literal, Pattern, TextPart};
use boon::platform::browser::kernel::KernelValue;
use boon_scene::UiEventKind;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct CounterProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub press_port: SourcePortId,
    pub counter_sink: SinkPortId,
    pub initial_value: i64,
    pub increment_delta: i64,
}

#[derive(Debug, Clone)]
pub struct ComplexCounterProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub decrement_port: SourcePortId,
    pub increment_port: SourcePortId,
    pub decrement_hovered_cell: MirrorCellId,
    pub increment_hovered_cell: MirrorCellId,
    pub counter_sink: SinkPortId,
    pub decrement_hovered_sink: SinkPortId,
    pub increment_hovered_sink: SinkPortId,
    pub initial_value: i64,
}

#[derive(Debug, Clone)]
pub struct ListRetainReactiveProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub toggle_port: SourcePortId,
    pub mode_sink: SinkPortId,
    pub count_sink: SinkPortId,
    pub items_list_sink: SinkPortId,
    pub item_sinks: [SinkPortId; 6],
}

#[derive(Debug, Clone)]
pub struct ListMapExternalDepProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub toggle_port: SourcePortId,
    pub mode_sink: SinkPortId,
    pub info_sink: SinkPortId,
    pub items_list_sink: SinkPortId,
    pub item_sinks: [SinkPortId; 4],
}

#[derive(Debug, Clone)]
pub struct ListMapBlockProgram {
    pub host_view: HostViewIr,
    pub mode_sink: SinkPortId,
    pub direct_item_sinks: [SinkPortId; 5],
    pub block_item_sinks: [SinkPortId; 5],
}

#[derive(Debug, Clone)]
pub struct ListRetainCountProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub input_sink: SinkPortId,
    pub all_count_sink: SinkPortId,
    pub retain_count_sink: SinkPortId,
    pub items_list_sink: SinkPortId,
    pub input_change_port: SourcePortId,
    pub input_key_down_port: SourcePortId,
    pub item_sinks: [SinkPortId; 4],
}

#[derive(Debug, Clone)]
pub struct ListRetainRemoveProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub title_sink: SinkPortId,
    pub input_sink: SinkPortId,
    pub count_sink: SinkPortId,
    pub items_list_sink: SinkPortId,
    pub input_change_port: SourcePortId,
    pub input_key_down_port: SourcePortId,
    pub item_sinks: [SinkPortId; 6],
}

#[derive(Debug, Clone)]
pub struct ListObjectStateProgram {
    pub host_view: HostViewIr,
    pub press_ports: [SourcePortId; 3],
    pub count_sinks: [SinkPortId; 3],
}

#[derive(Debug, Clone)]
pub struct ShoppingListProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub title_sink: SinkPortId,
    pub input_sink: SinkPortId,
    pub count_sink: SinkPortId,
    pub items_list_sink: SinkPortId,
    pub input_change_port: SourcePortId,
    pub input_key_down_port: SourcePortId,
    pub clear_press_port: SourcePortId,
    pub item_sinks: [SinkPortId; 4],
}

#[derive(Debug, Clone)]
pub struct FilterCheckboxBugProgram {
    pub host_view: HostViewIr,
    pub filter_all_port: SourcePortId,
    pub filter_active_port: SourcePortId,
    pub filter_sink: SinkPortId,
    pub checkbox_ports: [SourcePortId; 2],
    pub checkbox_sinks: [SinkPortId; 2],
    pub item_label_sinks: [SinkPortId; 2],
    pub footer_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct CheckboxTestProgram {
    pub host_view: HostViewIr,
    pub checkbox_ports: [SourcePortId; 2],
    pub checkbox_sinks: [SinkPortId; 2],
    pub label_sinks: [SinkPortId; 2],
    pub status_sinks: [SinkPortId; 2],
}

#[derive(Debug, Clone)]
pub struct ChainedListRemoveBugProgram {
    pub host_view: HostViewIr,
    pub add_press_port: SourcePortId,
    pub clear_completed_port: SourcePortId,
    pub checkbox_ports: [SourcePortId; 4],
    pub remove_ports: [SourcePortId; 4],
    pub checkbox_sinks: [SinkPortId; 4],
    pub row_label_sinks: [SinkPortId; 4],
    pub counts_sink: SinkPortId,
    pub title_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct CrudProgram {
    pub host_view: HostViewIr,
    pub title_sink: SinkPortId,
    pub filter_input_sink: SinkPortId,
    pub name_input_sink: SinkPortId,
    pub surname_input_sink: SinkPortId,
    pub filter_change_port: SourcePortId,
    pub filter_key_down_port: SourcePortId,
    pub name_change_port: SourcePortId,
    pub name_key_down_port: SourcePortId,
    pub surname_change_port: SourcePortId,
    pub surname_key_down_port: SourcePortId,
    pub create_press_port: SourcePortId,
    pub update_press_port: SourcePortId,
    pub delete_press_port: SourcePortId,
    pub row_press_ports: [SourcePortId; 4],
    pub row_label_sinks: [SinkPortId; 4],
    pub row_selected_sinks: [SinkPortId; 4],
}

#[derive(Debug, Clone)]
pub struct TemperatureConverterProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub title_sink: SinkPortId,
    pub celsius_input_sink: SinkPortId,
    pub fahrenheit_input_sink: SinkPortId,
    pub celsius_label_sink: SinkPortId,
    pub equals_label_sink: SinkPortId,
    pub fahrenheit_label_sink: SinkPortId,
    pub celsius_change_port: SourcePortId,
    pub celsius_key_down_port: SourcePortId,
    pub fahrenheit_change_port: SourcePortId,
    pub fahrenheit_key_down_port: SourcePortId,
}

#[derive(Debug, Clone)]
pub struct FlightBookerProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub title_sink: SinkPortId,
    pub selected_flight_type_sink: SinkPortId,
    pub departure_input_sink: SinkPortId,
    pub return_input_sink: SinkPortId,
    pub return_input_disabled_sink: SinkPortId,
    pub book_button_disabled_sink: SinkPortId,
    pub booked_sink: SinkPortId,
    pub flight_type_change_port: SourcePortId,
    pub departure_change_port: SourcePortId,
    pub return_change_port: SourcePortId,
    pub book_press_port: SourcePortId,
}

#[derive(Debug, Clone)]
pub struct TimerProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub title_sink: SinkPortId,
    pub elapsed_title_sink: SinkPortId,
    pub progress_percent_sink: SinkPortId,
    pub elapsed_value_sink: SinkPortId,
    pub duration_title_sink: SinkPortId,
    pub duration_slider_sink: SinkPortId,
    pub duration_value_sink: SinkPortId,
    pub duration_change_port: SourcePortId,
    pub reset_press_port: SourcePortId,
    pub tick_port: SourcePortId,
}

#[derive(Debug, Clone)]
pub struct IntervalProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub value_sink: SinkPortId,
    pub tick_port: SourcePortId,
    pub interval_ms: u32,
}

#[derive(Debug, Clone)]
pub struct FibonacciProgram {
    pub host_view: HostViewIr,
    pub sink_values: BTreeMap<SinkPortId, KernelValue>,
}

#[derive(Debug, Clone)]
pub struct LayersProgram {
    pub host_view: HostViewIr,
    pub sink_values: BTreeMap<SinkPortId, KernelValue>,
}

#[derive(Debug, Clone)]
pub struct PagesProgram {
    pub host_view: HostViewIr,
    pub nav_press_ports: [SourcePortId; 3],
    pub current_page_sink: SinkPortId,
    pub title_sink: SinkPortId,
    pub description_sink: SinkPortId,
    pub nav_active_sinks: [SinkPortId; 3],
}

#[derive(Debug, Clone)]
pub struct LatestProgram {
    pub host_view: HostViewIr,
    pub send_press_ports: [SourcePortId; 2],
    pub value_sink: SinkPortId,
    pub sum_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct TextInterpolationUpdateProgram {
    pub host_view: HostViewIr,
    pub toggle_press_port: SourcePortId,
    pub button_label_sink: SinkPortId,
    pub label_sink: SinkPortId,
    pub while_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct ThenProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub input_a_tick_port: SourcePortId,
    pub input_b_tick_port: SourcePortId,
    pub addition_press_port: SourcePortId,
    pub input_a_sink: SinkPortId,
    pub input_b_sink: SinkPortId,
    pub result_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct WhenProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub input_a_tick_port: SourcePortId,
    pub input_b_tick_port: SourcePortId,
    pub addition_press_port: SourcePortId,
    pub subtraction_press_port: SourcePortId,
    pub input_a_sink: SinkPortId,
    pub input_b_sink: SinkPortId,
    pub result_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct WhileProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub input_a_tick_port: SourcePortId,
    pub input_b_tick_port: SourcePortId,
    pub addition_press_port: SourcePortId,
    pub subtraction_press_port: SourcePortId,
    pub input_a_sink: SinkPortId,
    pub input_b_sink: SinkPortId,
    pub result_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct WhileFunctionCallProgram {
    pub host_view: HostViewIr,
    pub toggle_press_port: SourcePortId,
    pub toggle_label_sink: SinkPortId,
    pub content_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct ButtonHoverToClickTestProgram {
    pub host_view: HostViewIr,
    pub intro_sink: SinkPortId,
    pub button_press_ports: [SourcePortId; 3],
    pub button_active_sinks: [SinkPortId; 3],
    pub state_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct ButtonHoverTestProgram {
    pub host_view: HostViewIr,
    pub intro_sink: SinkPortId,
    pub button_press_ports: [SourcePortId; 3],
    pub button_hover_sinks: [SinkPortId; 3],
}

#[derive(Debug, Clone)]
pub struct SwitchHoldTestProgram {
    pub host_view: HostViewIr,
    pub show_item_a_sink: SinkPortId,
    pub item_count_sinks: [SinkPortId; 2],
    pub current_item_sink: SinkPortId,
    pub current_count_sink: SinkPortId,
    pub item_disabled_sinks: [SinkPortId; 2],
    pub footer_sink: SinkPortId,
    pub toggle_press_port: SourcePortId,
    pub item_press_ports: [SourcePortId; 2],
}

#[derive(Debug, Clone)]
pub struct CircleDrawerProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub title_sink: SinkPortId,
    pub count_sink: SinkPortId,
    pub circles_sink: SinkPortId,
    pub canvas_click_port: SourcePortId,
    pub undo_press_port: SourcePortId,
}

#[derive(Debug, Clone)]
pub struct StaticProgram {
    pub host_view: HostViewIr,
    pub sink_values: BTreeMap<SinkPortId, KernelValue>,
}

#[derive(Debug, Clone)]
pub struct TodoProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
    pub selected_filter_sink: SinkPortId,
}
pub struct TodoPhysicalProgram;

#[derive(Debug, Clone, Copy)]
pub struct CellsEditingView<'a> {
    pub row: u32,
    pub column: u32,
    pub draft: &'a str,
    pub focus_hint: bool,
}

#[derive(Debug, Clone)]
pub enum LoweredProgram {
    Counter(CounterProgram),
    ComplexCounter(ComplexCounterProgram),
    TodoMvc(TodoProgram),
    TodoMvcWithInitialTodos {
        program: TodoProgram,
        initial_todos: Vec<(u64, String, bool)>,
    },
    Interval(IntervalProgram),
    IntervalHold(IntervalProgram),
    Fibonacci(FibonacciProgram),
    Layers(LayersProgram),
    Pages(PagesProgram),
    Latest(LatestProgram),
    TextInterpolationUpdate(TextInterpolationUpdateProgram),
    ButtonHoverToClickTest(ButtonHoverToClickTestProgram),
    ButtonHoverTest(ButtonHoverTestProgram),
    FilterCheckboxBug(FilterCheckboxBugProgram),
    CheckboxTest(CheckboxTestProgram),
    TemperatureConverter(TemperatureConverterProgram),
    FlightBooker(FlightBookerProgram),
    Timer(TimerProgram),
    ListMapExternalDep(ListMapExternalDepProgram),
    ListMapBlock(ListMapBlockProgram),
    ListRetainCount(ListRetainCountProgram),
    ListObjectState(ListObjectStateProgram),
    ChainedListRemoveBug(ChainedListRemoveBugProgram),
    Crud(CrudProgram),
    ListRetainRemove(ListRetainRemoveProgram),
    ShoppingList(ShoppingListProgram),
    ListRetainReactive(ListRetainReactiveProgram),
    Then(ThenProgram),
    When(WhenProgram),
    While(WhileProgram),
    WhileFunctionCall(WhileFunctionCallProgram),
    SwitchHoldTest(SwitchHoldTestProgram),
    CircleDrawer(CircleDrawerProgram),
    Cells(CellsProgram),
    StaticDocument(StaticProgram),
}

impl LoweredProgram {
    pub fn into_host_view(self) -> Result<HostViewIr, String> {
        match self {
            Self::Counter(program) => Ok(program.host_view),
            Self::ComplexCounter(program) => Ok(program.host_view),
            Self::TodoMvc(program) | Self::TodoMvcWithInitialTodos { program, .. } => {
                Ok(program.host_view)
            }
            Self::Interval(program) => Ok(program.host_view),
            Self::IntervalHold(program) => Ok(program.host_view),
            Self::Fibonacci(program) => Ok(program.host_view),
            Self::Layers(program) => Ok(program.host_view),
            Self::Pages(program) => Ok(program.host_view),
            Self::Latest(program) => Ok(program.host_view),
            Self::TextInterpolationUpdate(program) => Ok(program.host_view),
            Self::ButtonHoverToClickTest(program) => Ok(program.host_view),
            Self::ButtonHoverTest(program) => Ok(program.host_view),
            Self::FilterCheckboxBug(program) => Ok(program.host_view),
            Self::CheckboxTest(program) => Ok(program.host_view),
            Self::TemperatureConverter(program) => Ok(program.host_view),
            Self::FlightBooker(program) => Ok(program.host_view),
            Self::Timer(program) => Ok(program.host_view),
            Self::ListMapExternalDep(program) => Ok(program.host_view),
            Self::ListMapBlock(program) => Ok(program.host_view),
            Self::ListRetainCount(program) => Ok(program.host_view),
            Self::ListObjectState(program) => Ok(program.host_view),
            Self::ChainedListRemoveBug(program) => Ok(program.host_view),
            Self::Crud(program) => Ok(program.host_view),
            Self::ListRetainRemove(program) => Ok(program.host_view),
            Self::ShoppingList(program) => Ok(program.host_view),
            Self::ListRetainReactive(program) => Ok(program.host_view),
            Self::Then(program) => Ok(program.host_view),
            Self::When(program) => Ok(program.host_view),
            Self::While(program) => Ok(program.host_view),
            Self::WhileFunctionCall(program) => Ok(program.host_view),
            Self::SwitchHoldTest(program) => Ok(program.host_view),
            Self::CircleDrawer(program) => Ok(program.host_view),
            Self::Cells(program) => Ok(program.initial_host_view()),
            Self::StaticDocument(program) => Ok(program.host_view),
        }
    }

    pub fn into_counter_program(self) -> Result<CounterProgram, String> {
        match self {
            Self::Counter(program) => Ok(program),
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not counter".to_string())
            }
            Self::TodoMvc(_) | Self::TodoMvcWithInitialTodos { .. } => {
                Err("generic lowerer matched todo_mvc, not counter".to_string())
            }
            Self::Interval(_) => Err("generic lowerer matched interval, not counter".to_string()),
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not counter".to_string())
            }
            Self::Fibonacci(_) => Err("generic lowerer matched fibonacci, not counter".to_string()),
            Self::Layers(_) => Err("generic lowerer matched layers, not counter".to_string()),
            Self::Pages(_) => Err("generic lowerer matched pages, not counter".to_string()),
            Self::Latest(_) => Err("generic lowerer matched latest, not counter".to_string()),
            Self::TextInterpolationUpdate(_) => {
                Err("generic lowerer matched text_interpolation_update, not counter".to_string())
            }
            Self::ButtonHoverToClickTest(_) => {
                Err("generic lowerer matched button_hover_to_click_test, not counter".to_string())
            }
            Self::ButtonHoverTest(_) => {
                Err("generic lowerer matched button_hover_test, not counter".to_string())
            }
            Self::FilterCheckboxBug(_) => {
                Err("generic lowerer matched filter_checkbox_bug, not counter".to_string())
            }
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not counter".to_string())
            }
            Self::TemperatureConverter(_) => {
                Err("generic lowerer matched temperature_converter, not counter".to_string())
            }
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not counter".to_string())
            }
            Self::Timer(_) => Err("generic lowerer matched timer, not counter".to_string()),
            Self::ListMapExternalDep(_) => {
                Err("generic lowerer matched list_map_external_dep, not counter".to_string())
            }
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not counter".to_string())
            }
            Self::ListRetainCount(_) => {
                Err("generic lowerer matched list_retain_count, not counter".to_string())
            }
            Self::ListObjectState(_) => {
                Err("generic lowerer matched list_object_state, not counter".to_string())
            }
            Self::ChainedListRemoveBug(_) => {
                Err("generic lowerer matched chained_list_remove_bug, not counter".to_string())
            }
            Self::Crud(_) => Err("generic lowerer matched crud, not counter".to_string()),
            Self::ListRetainRemove(_) => {
                Err("generic lowerer matched list_retain_remove, not counter".to_string())
            }
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not counter".to_string())
            }
            Self::ListRetainReactive(_) => {
                Err("generic lowerer matched list_retain_reactive, not counter".to_string())
            }
            Self::Then(_) => Err("generic lowerer matched then, not counter".to_string()),
            Self::When(_) => Err("generic lowerer matched when, not counter".to_string()),
            Self::While(_) => Err("generic lowerer matched while, not counter".to_string()),
            Self::WhileFunctionCall(_) => {
                Err("generic lowerer matched while_function_call, not counter".to_string())
            }
            Self::SwitchHoldTest(_) => {
                Err("generic lowerer matched switch_hold_test, not counter".to_string())
            }
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not counter".to_string())
            }
            Self::Cells(_) => Err("generic lowerer matched cells, not counter".to_string()),
            Self::StaticDocument(_) => {
                Err("generic lowerer matched static document, not counter".to_string())
            }
        }
    }

    pub fn into_todo_program(self) -> Result<TodoProgram, String> {
        match self {
            Self::TodoMvc(program) | Self::TodoMvcWithInitialTodos { program, .. } => Ok(program),
            Self::Counter(_) => Err("generic lowerer matched counter, not todo_mvc".to_string()),
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not todo_mvc".to_string())
            }
            Self::Interval(_) => Err("generic lowerer matched interval, not todo_mvc".to_string()),
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not todo_mvc".to_string())
            }
            Self::Fibonacci(_) => {
                Err("generic lowerer matched fibonacci, not todo_mvc".to_string())
            }
            Self::Layers(_) => Err("generic lowerer matched layers, not todo_mvc".to_string()),
            Self::Pages(_) => Err("generic lowerer matched pages, not todo_mvc".to_string()),
            Self::Latest(_) => Err("generic lowerer matched latest, not todo_mvc".to_string()),
            Self::TextInterpolationUpdate(_) => {
                Err("generic lowerer matched text_interpolation_update, not todo_mvc".to_string())
            }
            Self::ButtonHoverToClickTest(_) => {
                Err("generic lowerer matched button_hover_to_click_test, not todo_mvc".to_string())
            }
            Self::ButtonHoverTest(_) => {
                Err("generic lowerer matched button_hover_test, not todo_mvc".to_string())
            }
            Self::FilterCheckboxBug(_) => {
                Err("generic lowerer matched filter_checkbox_bug, not todo_mvc".to_string())
            }
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not todo_mvc".to_string())
            }
            Self::TemperatureConverter(_) => {
                Err("generic lowerer matched temperature_converter, not todo_mvc".to_string())
            }
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not todo_mvc".to_string())
            }
            Self::Timer(_) => Err("generic lowerer matched timer, not todo_mvc".to_string()),
            Self::ListMapExternalDep(_) => {
                Err("generic lowerer matched list_map_external_dep, not todo_mvc".to_string())
            }
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not todo_mvc".to_string())
            }
            Self::ListRetainCount(_) => {
                Err("generic lowerer matched list_retain_count, not todo_mvc".to_string())
            }
            Self::ListObjectState(_) => {
                Err("generic lowerer matched list_object_state, not todo_mvc".to_string())
            }
            Self::ChainedListRemoveBug(_) => {
                Err("generic lowerer matched chained_list_remove_bug, not todo_mvc".to_string())
            }
            Self::Crud(_) => Err("generic lowerer matched crud, not todo_mvc".to_string()),
            Self::ListRetainRemove(_) => {
                Err("generic lowerer matched list_retain_remove, not todo_mvc".to_string())
            }
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not todo_mvc".to_string())
            }
            Self::ListRetainReactive(_) => {
                Err("generic lowerer matched list_retain_reactive, not todo_mvc".to_string())
            }
            Self::Then(_) => Err("generic lowerer matched then, not todo_mvc".to_string()),
            Self::When(_) => Err("generic lowerer matched when, not todo_mvc".to_string()),
            Self::While(_) => Err("generic lowerer matched while, not todo_mvc".to_string()),
            Self::WhileFunctionCall(_) => {
                Err("generic lowerer matched while_function_call, not todo_mvc".to_string())
            }
            Self::SwitchHoldTest(_) => {
                Err("generic lowerer matched switch_hold_test, not todo_mvc".to_string())
            }
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not todo_mvc".to_string())
            }
            Self::Cells(_) => Err("generic lowerer matched cells, not todo_mvc".to_string()),
            Self::StaticDocument(_) => {
                Err("generic lowerer matched static document, not todo_mvc".to_string())
            }
        }
    }

    pub fn into_latest_program(self) -> Result<LatestProgram, String> {
        match self {
            Self::Latest(program) => Ok(program),
            Self::Counter(_) => Err("generic lowerer matched counter, not latest".to_string()),
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not latest".to_string())
            }
            Self::TodoMvc(_) | Self::TodoMvcWithInitialTodos { .. } => {
                Err("generic lowerer matched todo_mvc, not latest".to_string())
            }
            Self::Interval(_) => Err("generic lowerer matched interval, not latest".to_string()),
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not latest".to_string())
            }
            Self::Fibonacci(_) => Err("generic lowerer matched fibonacci, not latest".to_string()),
            Self::Layers(_) => Err("generic lowerer matched layers, not latest".to_string()),
            Self::Pages(_) => Err("generic lowerer matched pages, not latest".to_string()),
            Self::TextInterpolationUpdate(_) => {
                Err("generic lowerer matched text_interpolation_update, not latest".to_string())
            }
            Self::ButtonHoverToClickTest(_) => {
                Err("generic lowerer matched button_hover_to_click_test, not latest".to_string())
            }
            Self::ButtonHoverTest(_) => {
                Err("generic lowerer matched button_hover_test, not latest".to_string())
            }
            Self::FilterCheckboxBug(_) => {
                Err("generic lowerer matched filter_checkbox_bug, not latest".to_string())
            }
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not latest".to_string())
            }
            Self::TemperatureConverter(_) => {
                Err("generic lowerer matched temperature_converter, not latest".to_string())
            }
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not latest".to_string())
            }
            Self::Timer(_) => Err("generic lowerer matched timer, not latest".to_string()),
            Self::ListMapExternalDep(_) => {
                Err("generic lowerer matched list_map_external_dep, not latest".to_string())
            }
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not latest".to_string())
            }
            Self::ListRetainCount(_) => {
                Err("generic lowerer matched list_retain_count, not latest".to_string())
            }
            Self::ListObjectState(_) => {
                Err("generic lowerer matched list_object_state, not latest".to_string())
            }
            Self::ChainedListRemoveBug(_) => {
                Err("generic lowerer matched chained_list_remove_bug, not latest".to_string())
            }
            Self::Crud(_) => Err("generic lowerer matched crud, not latest".to_string()),
            Self::ListRetainRemove(_) => {
                Err("generic lowerer matched list_retain_remove, not latest".to_string())
            }
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not latest".to_string())
            }
            Self::ListRetainReactive(_) => {
                Err("generic lowerer matched list_retain_reactive, not latest".to_string())
            }
            Self::Then(_) => Err("generic lowerer matched then, not latest".to_string()),
            Self::When(_) => Err("generic lowerer matched when, not latest".to_string()),
            Self::While(_) => Err("generic lowerer matched while, not latest".to_string()),
            Self::WhileFunctionCall(_) => {
                Err("generic lowerer matched while_function_call, not latest".to_string())
            }
            Self::SwitchHoldTest(_) => {
                Err("generic lowerer matched switch_hold_test, not latest".to_string())
            }
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not latest".to_string())
            }
            Self::Cells(_) => Err("generic lowerer matched cells, not latest".to_string()),
            Self::StaticDocument(_) => {
                Err("generic lowerer matched static document, not latest".to_string())
            }
        }
    }

    pub fn into_then_program(self) -> Result<ThenProgram, String> {
        match self {
            Self::Then(program) => Ok(program),
            Self::Counter(_) => Err("generic lowerer matched counter, not then".to_string()),
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not then".to_string())
            }
            Self::TodoMvc(_) | Self::TodoMvcWithInitialTodos { .. } => {
                Err("generic lowerer matched todo_mvc, not then".to_string())
            }
            Self::Interval(_) => Err("generic lowerer matched interval, not then".to_string()),
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not then".to_string())
            }
            Self::Fibonacci(_) => Err("generic lowerer matched fibonacci, not then".to_string()),
            Self::Layers(_) => Err("generic lowerer matched layers, not then".to_string()),
            Self::Pages(_) => Err("generic lowerer matched pages, not then".to_string()),
            Self::Latest(_) => Err("generic lowerer matched latest, not then".to_string()),
            Self::TextInterpolationUpdate(_) => {
                Err("generic lowerer matched text_interpolation_update, not then".to_string())
            }
            Self::ButtonHoverToClickTest(_) => {
                Err("generic lowerer matched button_hover_to_click_test, not then".to_string())
            }
            Self::ButtonHoverTest(_) => {
                Err("generic lowerer matched button_hover_test, not then".to_string())
            }
            Self::FilterCheckboxBug(_) => {
                Err("generic lowerer matched filter_checkbox_bug, not then".to_string())
            }
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not then".to_string())
            }
            Self::TemperatureConverter(_) => {
                Err("generic lowerer matched temperature_converter, not then".to_string())
            }
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not then".to_string())
            }
            Self::Timer(_) => Err("generic lowerer matched timer, not then".to_string()),
            Self::ListMapExternalDep(_) => {
                Err("generic lowerer matched list_map_external_dep, not then".to_string())
            }
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not then".to_string())
            }
            Self::ListRetainCount(_) => {
                Err("generic lowerer matched list_retain_count, not then".to_string())
            }
            Self::ListObjectState(_) => {
                Err("generic lowerer matched list_object_state, not then".to_string())
            }
            Self::ChainedListRemoveBug(_) => {
                Err("generic lowerer matched chained_list_remove_bug, not then".to_string())
            }
            Self::Crud(_) => Err("generic lowerer matched crud, not then".to_string()),
            Self::ListRetainRemove(_) => {
                Err("generic lowerer matched list_retain_remove, not then".to_string())
            }
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not then".to_string())
            }
            Self::ListRetainReactive(_) => {
                Err("generic lowerer matched list_retain_reactive, not then".to_string())
            }
            Self::When(_) => Err("generic lowerer matched when, not then".to_string()),
            Self::While(_) => Err("generic lowerer matched while, not then".to_string()),
            Self::WhileFunctionCall(_) => {
                Err("generic lowerer matched while_function_call, not then".to_string())
            }
            Self::SwitchHoldTest(_) => {
                Err("generic lowerer matched switch_hold_test, not then".to_string())
            }
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not then".to_string())
            }
            Self::Cells(_) => Err("generic lowerer matched cells, not then".to_string()),
            Self::StaticDocument(_) => {
                Err("generic lowerer matched static document, not then".to_string())
            }
        }
    }

    pub fn into_list_retain_reactive_program(self) -> Result<ListRetainReactiveProgram, String> {
        match self {
            Self::ListRetainReactive(program) => Ok(program),
            Self::Counter(_) => {
                Err("generic lowerer matched counter, not list_retain_reactive".to_string())
            }
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not list_retain_reactive".to_string())
            }
            Self::TodoMvc(_) | Self::TodoMvcWithInitialTodos { .. } => {
                Err("generic lowerer matched todo_mvc, not list_retain_reactive".to_string())
            }
            Self::Interval(_) => {
                Err("generic lowerer matched interval, not list_retain_reactive".to_string())
            }
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not list_retain_reactive".to_string())
            }
            Self::Fibonacci(_) => {
                Err("generic lowerer matched fibonacci, not list_retain_reactive".to_string())
            }
            Self::Layers(_) => {
                Err("generic lowerer matched layers, not list_retain_reactive".to_string())
            }
            Self::Pages(_) => {
                Err("generic lowerer matched pages, not list_retain_reactive".to_string())
            }
            Self::Latest(_) => {
                Err("generic lowerer matched latest, not list_retain_reactive".to_string())
            }
            Self::TextInterpolationUpdate(_) => Err(
                "generic lowerer matched text_interpolation_update, not list_retain_reactive"
                    .to_string(),
            ),
            Self::ButtonHoverToClickTest(_) => Err(
                "generic lowerer matched button_hover_to_click_test, not list_retain_reactive"
                    .to_string(),
            ),
            Self::ButtonHoverTest(_) => Err(
                "generic lowerer matched button_hover_test, not list_retain_reactive".to_string(),
            ),
            Self::FilterCheckboxBug(_) => Err(
                "generic lowerer matched filter_checkbox_bug, not list_retain_reactive".to_string(),
            ),
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not list_retain_reactive".to_string())
            }
            Self::TemperatureConverter(_) => Err(
                "generic lowerer matched temperature_converter, not list_retain_reactive"
                    .to_string(),
            ),
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not list_retain_reactive".to_string())
            }
            Self::Timer(_) => {
                Err("generic lowerer matched timer, not list_retain_reactive".to_string())
            }
            Self::ListMapExternalDep(_) => Err(
                "generic lowerer matched list_map_external_dep, not list_retain_reactive"
                    .to_string(),
            ),
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not list_retain_reactive".to_string())
            }
            Self::ListRetainCount(_) => Err(
                "generic lowerer matched list_retain_count, not list_retain_reactive".to_string(),
            ),
            Self::ListObjectState(_) => Err(
                "generic lowerer matched list_object_state, not list_retain_reactive".to_string(),
            ),
            Self::ChainedListRemoveBug(_) => Err(
                "generic lowerer matched chained_list_remove_bug, not list_retain_reactive"
                    .to_string(),
            ),
            Self::Crud(_) => {
                Err("generic lowerer matched crud, not list_retain_reactive".to_string())
            }
            Self::ListRetainRemove(_) => Err(
                "generic lowerer matched list_retain_remove, not list_retain_reactive".to_string(),
            ),
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not list_retain_reactive".to_string())
            }
            Self::Then(_) => {
                Err("generic lowerer matched then, not list_retain_reactive".to_string())
            }
            Self::When(_) => {
                Err("generic lowerer matched when, not list_retain_reactive".to_string())
            }
            Self::While(_) => {
                Err("generic lowerer matched while, not list_retain_reactive".to_string())
            }
            Self::WhileFunctionCall(_) => Err(
                "generic lowerer matched while_function_call, not list_retain_reactive".to_string(),
            ),
            Self::SwitchHoldTest(_) => Err(
                "generic lowerer matched switch_hold_test, not list_retain_reactive".to_string(),
            ),
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not list_retain_reactive".to_string())
            }
            Self::Cells(_) => {
                Err("generic lowerer matched cells, not list_retain_reactive".to_string())
            }
            Self::StaticDocument(_) => {
                Err("generic lowerer matched static document, not list_retain_reactive".to_string())
            }
        }
    }

    pub fn into_when_program(self) -> Result<WhenProgram, String> {
        match self {
            Self::When(program) => Ok(program),
            Self::Counter(_) => Err("generic lowerer matched counter, not when".to_string()),
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not when".to_string())
            }
            Self::TodoMvc(_) | Self::TodoMvcWithInitialTodos { .. } => {
                Err("generic lowerer matched todo_mvc, not when".to_string())
            }
            Self::Interval(_) => Err("generic lowerer matched interval, not when".to_string()),
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not when".to_string())
            }
            Self::Fibonacci(_) => Err("generic lowerer matched fibonacci, not when".to_string()),
            Self::Layers(_) => Err("generic lowerer matched layers, not when".to_string()),
            Self::Pages(_) => Err("generic lowerer matched pages, not when".to_string()),
            Self::Latest(_) => Err("generic lowerer matched latest, not when".to_string()),
            Self::TextInterpolationUpdate(_) => {
                Err("generic lowerer matched text_interpolation_update, not when".to_string())
            }
            Self::ButtonHoverToClickTest(_) => {
                Err("generic lowerer matched button_hover_to_click_test, not when".to_string())
            }
            Self::ButtonHoverTest(_) => {
                Err("generic lowerer matched button_hover_test, not when".to_string())
            }
            Self::FilterCheckboxBug(_) => {
                Err("generic lowerer matched filter_checkbox_bug, not when".to_string())
            }
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not when".to_string())
            }
            Self::TemperatureConverter(_) => {
                Err("generic lowerer matched temperature_converter, not when".to_string())
            }
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not when".to_string())
            }
            Self::Timer(_) => Err("generic lowerer matched timer, not when".to_string()),
            Self::ListMapExternalDep(_) => {
                Err("generic lowerer matched list_map_external_dep, not when".to_string())
            }
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not when".to_string())
            }
            Self::ListRetainCount(_) => {
                Err("generic lowerer matched list_retain_count, not when".to_string())
            }
            Self::ListObjectState(_) => {
                Err("generic lowerer matched list_object_state, not when".to_string())
            }
            Self::ChainedListRemoveBug(_) => {
                Err("generic lowerer matched chained_list_remove_bug, not when".to_string())
            }
            Self::Crud(_) => Err("generic lowerer matched crud, not when".to_string()),
            Self::ListRetainRemove(_) => {
                Err("generic lowerer matched list_retain_remove, not when".to_string())
            }
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not when".to_string())
            }
            Self::ListRetainReactive(_) => {
                Err("generic lowerer matched list_retain_reactive, not when".to_string())
            }
            Self::Then(_) => Err("generic lowerer matched then, not when".to_string()),
            Self::While(_) => Err("generic lowerer matched while, not when".to_string()),
            Self::WhileFunctionCall(_) => {
                Err("generic lowerer matched while_function_call, not when".to_string())
            }
            Self::SwitchHoldTest(_) => {
                Err("generic lowerer matched switch_hold_test, not when".to_string())
            }
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not when".to_string())
            }
            Self::Cells(_) => Err("generic lowerer matched cells, not when".to_string()),
            Self::StaticDocument(_) => {
                Err("generic lowerer matched static document, not when".to_string())
            }
        }
    }

    pub fn into_while_program(self) -> Result<WhileProgram, String> {
        match self {
            Self::While(program) => Ok(program),
            Self::Counter(_) => Err("generic lowerer matched counter, not while".to_string()),
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not while".to_string())
            }
            Self::TodoMvc(_) | Self::TodoMvcWithInitialTodos { .. } => {
                Err("generic lowerer matched todo_mvc, not while".to_string())
            }
            Self::Interval(_) => Err("generic lowerer matched interval, not while".to_string()),
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not while".to_string())
            }
            Self::Fibonacci(_) => Err("generic lowerer matched fibonacci, not while".to_string()),
            Self::Layers(_) => Err("generic lowerer matched layers, not while".to_string()),
            Self::Pages(_) => Err("generic lowerer matched pages, not while".to_string()),
            Self::Latest(_) => Err("generic lowerer matched latest, not while".to_string()),
            Self::TextInterpolationUpdate(_) => {
                Err("generic lowerer matched text_interpolation_update, not while".to_string())
            }
            Self::ButtonHoverToClickTest(_) => {
                Err("generic lowerer matched button_hover_to_click_test, not while".to_string())
            }
            Self::ButtonHoverTest(_) => {
                Err("generic lowerer matched button_hover_test, not while".to_string())
            }
            Self::FilterCheckboxBug(_) => {
                Err("generic lowerer matched filter_checkbox_bug, not while".to_string())
            }
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not while".to_string())
            }
            Self::TemperatureConverter(_) => {
                Err("generic lowerer matched temperature_converter, not while".to_string())
            }
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not while".to_string())
            }
            Self::Timer(_) => Err("generic lowerer matched timer, not while".to_string()),
            Self::ListMapExternalDep(_) => {
                Err("generic lowerer matched list_map_external_dep, not while".to_string())
            }
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not while".to_string())
            }
            Self::ListRetainCount(_) => {
                Err("generic lowerer matched list_retain_count, not while".to_string())
            }
            Self::ListObjectState(_) => {
                Err("generic lowerer matched list_object_state, not while".to_string())
            }
            Self::ChainedListRemoveBug(_) => {
                Err("generic lowerer matched chained_list_remove_bug, not while".to_string())
            }
            Self::Crud(_) => Err("generic lowerer matched crud, not while".to_string()),
            Self::ListRetainRemove(_) => {
                Err("generic lowerer matched list_retain_remove, not while".to_string())
            }
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not while".to_string())
            }
            Self::ListRetainReactive(_) => {
                Err("generic lowerer matched list_retain_reactive, not while".to_string())
            }
            Self::Then(_) => Err("generic lowerer matched then, not while".to_string()),
            Self::When(_) => Err("generic lowerer matched when, not while".to_string()),
            Self::WhileFunctionCall(_) => {
                Err("generic lowerer matched while_function_call, not while".to_string())
            }
            Self::SwitchHoldTest(_) => {
                Err("generic lowerer matched switch_hold_test, not while".to_string())
            }
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not while".to_string())
            }
            Self::Cells(_) => Err("generic lowerer matched cells, not while".to_string()),
            Self::StaticDocument(_) => {
                Err("generic lowerer matched static document, not while".to_string())
            }
        }
    }

    pub fn into_static_program(self) -> Result<StaticProgram, String> {
        match self {
            Self::StaticDocument(program) => Ok(program),
            Self::Counter(_) => {
                Err("generic lowerer matched counter, not static document".to_string())
            }
            Self::ComplexCounter(_) => {
                Err("generic lowerer matched complex_counter, not static document".to_string())
            }
            Self::TodoMvc(_) | Self::TodoMvcWithInitialTodos { .. } => {
                Err("generic lowerer matched todo_mvc, not static document".to_string())
            }
            Self::Interval(_) => {
                Err("generic lowerer matched interval, not static document".to_string())
            }
            Self::IntervalHold(_) => {
                Err("generic lowerer matched interval_hold, not static document".to_string())
            }
            Self::Fibonacci(_) => {
                Err("generic lowerer matched fibonacci, not static document".to_string())
            }
            Self::Layers(_) => {
                Err("generic lowerer matched layers, not static document".to_string())
            }
            Self::Pages(_) => Err("generic lowerer matched pages, not static document".to_string()),
            Self::Latest(_) => {
                Err("generic lowerer matched latest, not static document".to_string())
            }
            Self::TextInterpolationUpdate(_) => Err(
                "generic lowerer matched text_interpolation_update, not static document"
                    .to_string(),
            ),
            Self::ButtonHoverToClickTest(_) => Err(
                "generic lowerer matched button_hover_to_click_test, not static document"
                    .to_string(),
            ),
            Self::ButtonHoverTest(_) => {
                Err("generic lowerer matched button_hover_test, not static document".to_string())
            }
            Self::FilterCheckboxBug(_) => {
                Err("generic lowerer matched filter_checkbox_bug, not static document".to_string())
            }
            Self::CheckboxTest(_) => {
                Err("generic lowerer matched checkbox_test, not static document".to_string())
            }
            Self::TemperatureConverter(_) => Err(
                "generic lowerer matched temperature_converter, not static document".to_string(),
            ),
            Self::FlightBooker(_) => {
                Err("generic lowerer matched flight_booker, not static document".to_string())
            }
            Self::Timer(_) => Err("generic lowerer matched timer, not static document".to_string()),
            Self::ListMapExternalDep(_) => Err(
                "generic lowerer matched list_map_external_dep, not static document".to_string(),
            ),
            Self::ListMapBlock(_) => {
                Err("generic lowerer matched list_map_block, not static document".to_string())
            }
            Self::ListRetainCount(_) => {
                Err("generic lowerer matched list_retain_count, not static document".to_string())
            }
            Self::ListObjectState(_) => {
                Err("generic lowerer matched list_object_state, not static document".to_string())
            }
            Self::ChainedListRemoveBug(_) => Err(
                "generic lowerer matched chained_list_remove_bug, not static document".to_string(),
            ),
            Self::Crud(_) => Err("generic lowerer matched crud, not static document".to_string()),
            Self::ListRetainRemove(_) => {
                Err("generic lowerer matched list_retain_remove, not static document".to_string())
            }
            Self::ShoppingList(_) => {
                Err("generic lowerer matched shopping_list, not static document".to_string())
            }
            Self::ListRetainReactive(_) => {
                Err("generic lowerer matched list_retain_reactive, not static document".to_string())
            }
            Self::Then(_) => Err("generic lowerer matched then, not static document".to_string()),
            Self::When(_) => Err("generic lowerer matched when, not static document".to_string()),
            Self::While(_) => Err("generic lowerer matched while, not static document".to_string()),
            Self::WhileFunctionCall(_) => {
                Err("generic lowerer matched while_function_call, not static document".to_string())
            }
            Self::SwitchHoldTest(_) => {
                Err("generic lowerer matched switch_hold_test, not static document".to_string())
            }
            Self::CircleDrawer(_) => {
                Err("generic lowerer matched circle_drawer, not static document".to_string())
            }
            Self::Cells(_) => Err("generic lowerer matched cells, not static document".to_string()),
        }
    }
}

fn lowered_program_subset(program: &LoweredProgram) -> &'static str {
    match program {
        LoweredProgram::Counter(_) => "single_action_accumulator_document",
        LoweredProgram::ComplexCounter(_) => "dual_action_accumulator_document",
        LoweredProgram::TodoMvc(_) | LoweredProgram::TodoMvcWithInitialTodos { .. } => {
            "editable_filterable_list_document"
        }
        LoweredProgram::Interval(_) => "summed_interval_signal_document",
        LoweredProgram::IntervalHold(_) => "held_interval_signal_document",
        LoweredProgram::Fibonacci(_) => "sequence_message_display",
        LoweredProgram::Layers(_) => "static_stack_display",
        LoweredProgram::Pages(_) => "nav_selection_document",
        LoweredProgram::Latest(_) => "latest_signal_document",
        LoweredProgram::TextInterpolationUpdate(_) => "toggle_templated_label_document",
        LoweredProgram::ButtonHoverToClickTest(_) => "multi_button_activation_document",
        LoweredProgram::ButtonHoverTest(_) => "multi_button_hover_document",
        LoweredProgram::FilterCheckboxBug(_) => "filterable_checkbox_list_document",
        LoweredProgram::CheckboxTest(_) => "independent_checkbox_list_document",
        LoweredProgram::TemperatureConverter(_) => "bidirectional_conversion_form_document",
        LoweredProgram::FlightBooker(_) => "selectable_dual_date_form_document",
        LoweredProgram::Timer(_) => "resettable_timed_progress_document",
        LoweredProgram::ListMapExternalDep(_) => "external_mode_mapped_items_document",
        LoweredProgram::ListMapBlock(_) => "dual_mapped_label_stripes_document",
        LoweredProgram::ListRetainCount(_) => "counted_filtered_append_list_document",
        LoweredProgram::ListObjectState(_) => "independent_object_counters_document",
        LoweredProgram::ChainedListRemoveBug(_) => "removable_checkbox_list_document",
        LoweredProgram::Crud(_) => "selectable_record_column_document",
        LoweredProgram::ListRetainRemove(_) => "removable_append_list_document",
        LoweredProgram::ShoppingList(_) => "clearable_append_list_document",
        LoweredProgram::ListRetainReactive(_) => "retained_toggle_filter_list_document",
        LoweredProgram::Then(_) => "timed_addition_hold_document",
        LoweredProgram::When(_) => "timed_operation_hold_document",
        LoweredProgram::While(_) => "timed_operation_stream_document",
        LoweredProgram::WhileFunctionCall(_) => "toggle_branch_document",
        LoweredProgram::SwitchHoldTest(_) => "switched_hold_items_document",
        LoweredProgram::CircleDrawer(_) => "canvas_history_document",
        LoweredProgram::Cells(_) => "persistent_indexed_text_grid_document",
        LoweredProgram::StaticDocument(_) => "static_document_display",
    }
}

impl TodoProgram {
    pub const MAIN_INPUT_CHANGE_PORT: SourcePortId = SourcePortId(100);
    pub const MAIN_INPUT_KEY_DOWN_PORT: SourcePortId = SourcePortId(101);
    pub const MAIN_INPUT_BLUR_PORT: SourcePortId = SourcePortId(102);
    pub const MAIN_INPUT_FOCUS_PORT: SourcePortId = SourcePortId(103);
    pub const MAIN_INPUT_DRAFT_CELL: MirrorCellId = MirrorCellId(100);
    pub const MAIN_INPUT_FOCUSED_CELL: MirrorCellId = MirrorCellId(101);
    pub const TODOS_LIST_CELL: MirrorCellId = MirrorCellId(102);
    pub const NEXT_TODO_ID_CELL: MirrorCellId = MirrorCellId(103);
    pub const EDIT_TITLE_CELL: MirrorCellId = MirrorCellId(105);
    pub const EDIT_FOCUSED_CELL: MirrorCellId = MirrorCellId(106);
    pub const MAIN_INPUT_FOCUS_HINT_CELL: MirrorCellId = MirrorCellId(108);
    pub const TODOS_LIST_HOLD_NODE: NodeId = NodeId(1430);
    pub const SELECTED_FILTER_SINK: SinkPortId = SinkPortId(140);
    pub const MAIN_INPUT_TEXT_SINK: SinkPortId = SinkPortId(141);
    pub const MAIN_INPUT_FOCUSED_SINK: SinkPortId = SinkPortId(142);
    pub const TODOS_LIST_SINK: SinkPortId = SinkPortId(143);
    pub const EDIT_TARGET_SINK: SinkPortId = SinkPortId(144);
    pub const EDIT_DRAFT_SINK: SinkPortId = SinkPortId(145);
    pub const EDIT_FOCUS_HINT_SINK: SinkPortId = SinkPortId(146);
    pub const EDIT_FOCUSED_SINK: SinkPortId = SinkPortId(147);
    pub const HOVERED_TARGET_SINK: SinkPortId = SinkPortId(148);
    pub const ACTIVE_COUNT_SINK: SinkPortId = SinkPortId(149);
    pub const COMPLETED_COUNT_SINK: SinkPortId = SinkPortId(150);
    pub const ALL_COMPLETED_SINK: SinkPortId = SinkPortId(151);
    pub const MAIN_INPUT_FOCUS_HINT_SINK: SinkPortId = SinkPortId(152);
    pub const VISIBLE_TODOS_SINK: SinkPortId = SinkPortId(4_990);
    pub const ACTIVE_COUNT_LABEL_SINK: SinkPortId = SinkPortId(4_991);
    pub const FILTER_ALL_OUTLINE_SINK: SinkPortId = SinkPortId(4_992);
    pub const FILTER_ACTIVE_OUTLINE_SINK: SinkPortId = SinkPortId(4_993);
    pub const FILTER_COMPLETED_OUTLINE_SINK: SinkPortId = SinkPortId(4_994);
    pub const FILTER_ALL_PORT: SourcePortId = SourcePortId(110);
    pub const FILTER_ACTIVE_PORT: SourcePortId = SourcePortId(111);
    pub const FILTER_COMPLETED_PORT: SourcePortId = SourcePortId(112);
    pub const TOGGLE_ALL_PORT: SourcePortId = SourcePortId(120);
    pub const CLEAR_COMPLETED_PORT: SourcePortId = SourcePortId(121);
    pub const TODO_TOGGLE_PORT: SourcePortId = SourcePortId(122);
    pub const TODO_DELETE_PORT: SourcePortId = SourcePortId(123);
    pub const TODO_EDIT_COMMIT_PORT: SourcePortId = SourcePortId(124);
    pub const TODO_BEGIN_EDIT_PORT: SourcePortId = SourcePortId(125);
    pub const TODO_EDIT_CANCEL_PORT: SourcePortId = SourcePortId(126);
    pub const TODO_EDIT_BLUR_PORT: SourcePortId = SourcePortId(127);
    pub const TODO_EDIT_FOCUS_PORT: SourcePortId = SourcePortId(128);
    pub const TODO_EDIT_CHANGE_PORT: SourcePortId = SourcePortId(129);
    pub const TODO_HOVER_PORT: SourcePortId = SourcePortId(130);

    pub fn host_view_sink_values(
        &self,
        base_sink_values: &BTreeMap<SinkPortId, KernelValue>,
    ) -> BTreeMap<SinkPortId, KernelValue> {
        derive_editable_filterable_list_host_view_sink_values(base_sink_values)
    }

    pub fn materialize_host_view(
        &self,
        sink_values: &BTreeMap<SinkPortId, KernelValue>,
    ) -> Result<HostViewIr, String> {
        materialize_host_view_from_derived_sink_values(
            sink_values,
            derive_editable_filterable_list_host_view_sink_values,
            editable_filterable_list_host_view_template(),
        )
    }
}

impl TodoPhysicalProgram {
    pub const MAIN_INPUT_CHANGE_PORT: SourcePortId = SourcePortId(8_100);
    pub const MAIN_INPUT_KEY_DOWN_PORT: SourcePortId = SourcePortId(8_101);
    pub const MAIN_INPUT_BLUR_PORT: SourcePortId = SourcePortId(8_102);
    pub const MAIN_INPUT_FOCUS_PORT: SourcePortId = SourcePortId(8_103);
    pub const FILTER_ALL_PORT: SourcePortId = SourcePortId(8_110);
    pub const FILTER_ACTIVE_PORT: SourcePortId = SourcePortId(8_111);
    pub const FILTER_COMPLETED_PORT: SourcePortId = SourcePortId(8_112);
    pub const TOGGLE_ALL_PORT: SourcePortId = SourcePortId(8_120);
    pub const CLEAR_COMPLETED_PORT: SourcePortId = SourcePortId(8_121);
    pub const THEME_PROFESSIONAL_PORT: SourcePortId = SourcePortId(8_130);
    pub const THEME_GLASS_PORT: SourcePortId = SourcePortId(8_131);
    pub const THEME_BRUTALIST_PORT: SourcePortId = SourcePortId(8_132);
    pub const THEME_NEUMORPHIC_PORT: SourcePortId = SourcePortId(8_133);
    pub const TOGGLE_MODE_PORT: SourcePortId = SourcePortId(8_134);
}

fn editable_filterable_list_seed_items_value() -> KernelValue {
    KernelValue::List(vec![
        KernelValue::Object(BTreeMap::from([
            ("id".to_string(), KernelValue::from(1.0)),
            ("title".to_string(), KernelValue::from("Buy groceries")),
            ("completed".to_string(), KernelValue::from(false)),
        ])),
        KernelValue::Object(BTreeMap::from([
            ("id".to_string(), KernelValue::from(2.0)),
            ("title".to_string(), KernelValue::from("Clean room")),
            ("completed".to_string(), KernelValue::from(false)),
        ])),
    ])
}

impl CellsProgram {
    pub const HEADING_CLICK_PORT: SourcePortId = SourcePortId(4_000);

    #[must_use]
    pub fn initial_host_view(&self) -> HostViewIr {
        let sheet = CellsSheetState::new_lowered(
            self.default_formulas.clone(),
            self.baseline_state.clone(),
        );
        self.materialize_host_view(&sheet, None)
    }

    #[must_use]
    pub fn materialize_host_view(
        &self,
        sheet: &CellsSheetState,
        editing: Option<CellsEditingView<'_>>,
    ) -> HostViewIr {
        HostViewIr {
            root: Some(indexed_grid_root_node(self, sheet, editing)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexedStaticValue {
    Number(i64),
    Bool(bool),
    Text(String),
}

#[derive(Clone, Copy)]
struct IndexedStaticExpressionConfig<'a> {
    subset: &'static str,
    context_label: &'static str,
    row_aliases: &'a [&'a str],
    column_aliases: &'a [&'a str],
    empty_text_function_path: &'a [&'a str],
}

#[derive(Clone, Copy)]
enum IndexedTextGridRowRange {
    Fixed(u32),
    InclusiveFromOneToRowCount,
}

fn map_indexed_grid_formula_text(text: String) -> Option<LoweredCellsFormula> {
    (!text.is_empty()).then(|| parse_lowered_cells_formula(text))
}

fn build_indexed_grid_formula_state(
    grid: &BTreeMap<(u32, u32), LoweredCellsFormula>,
) -> CellsFormulaState {
    CellsFormulaState::from_lowered_formulas(
        grid.iter()
            .map(|(coords, formula)| (*coords, formula.clone())),
    )
}

#[derive(Clone, Copy)]
enum IndexedTextGridLoweredProgramSpec {
    Cells,
}

fn build_indexed_text_grid_lowered_program(
    ir: IrProgram,
    semantic: IndexedTextGridSemantic<LoweredCellsFormula, CellsFormulaState>,
    spec: IndexedTextGridLoweredProgramSpec,
) -> LoweredProgram {
    match spec {
        IndexedTextGridLoweredProgramSpec::Cells => LoweredProgram::Cells(CellsProgram {
            ir,
            title: semantic.document.title,
            display_title: semantic.document.display_title,
            row_count: semantic.document.row_count,
            col_count: semantic.document.col_count,
            column_headers: semantic.document.column_headers,
            default_formulas: semantic.grid,
            baseline_state: semantic.state,
        }),
    }
}

fn build_persistent_indexed_text_grid_output(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    ir: IrProgram,
    config: &IndexedTextGridProgramConfig<'_>,
) -> Result<LoweredProgram, String> {
    let semantic = derive_indexed_text_grid_semantic(
        expressions,
        bindings,
        config.semantic.document.expression.subset,
        &config.semantic,
    )?;

    Ok(build_indexed_text_grid_lowered_program(
        ir,
        semantic,
        config.program,
    ))
}

fn derive_persistent_semantic_ir<S, T>(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &PersistentSemanticOutputConfig<'_, S, T>,
) -> Result<IrProgram, String> {
    require_structural_validation(expressions, bindings, &config.validation)?;
    Ok((config.build_ir)(
        collect_path_lowering_persistence_from_configs(bindings, config.persistence),
    ))
}

fn build_persistent_semantic_output<S, T>(
    expressions: &[StaticSpannedExpression],
    bindings: BTreeMap<String, &StaticSpannedExpression>,
    ir: IrProgram,
    config: &PersistentSemanticOutputConfig<'_, S, T>,
) -> Result<T, String> {
    (config.build_output)(expressions, &bindings, ir, &config.semantic)
}

fn derive_indexed_text_grid_semantic<T, S>(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    subset: &str,
    config: &IndexedTextGridSemanticFamilyConfig<'_, T, S>,
) -> Result<IndexedTextGridSemantic<T, S>, String> {
    let document =
        derive_indexed_text_grid_document(expressions, bindings, subset, &config.document)?;
    let (grid, state) = derive_mapped_indexed_text_grid_state(
        expressions,
        document.row_count,
        document.col_count,
        subset,
        &config.document.expression,
        config.grid,
        config.map_value,
        config.build_state,
    )?;

    Ok(IndexedTextGridSemantic {
        document,
        grid,
        state,
    })
}

fn derive_indexed_text_grid_document(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    subset: &str,
    config: &IndexedTextGridDocumentConfig<'_>,
) -> Result<IndexedTextGridDocument, String> {
    let row_count = derive_defaulted_literal_u32_binding(bindings, config.row_count);
    let col_count = derive_defaulted_literal_u32_binding(bindings, config.col_count);
    let title = select_static_or_dynamic_title(
        bindings,
        config.dynamic_title_bindings,
        config.static_title,
        config.dynamic_title,
    );
    let display_title = extract_optional_text_from_top_level_binding(
        bindings,
        subset,
        config.document_title_binding_name,
        config.document_title_binding_path,
        config.document_title_steps,
    )?
    .unwrap_or_else(|| title.to_string());
    let column_headers = collect_mapped_indexed_text_grid_values(
        expressions,
        row_count,
        col_count,
        subset,
        &config.expression,
        config.column_headers,
        Some,
    )?;

    Ok(IndexedTextGridDocument {
        title,
        display_title,
        row_count,
        col_count,
        column_headers,
    })
}

fn derive_defaulted_literal_u32_binding(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    spec: LiteralU32BindingSpec<'_>,
) -> u32 {
    bindings
        .get(spec.binding_name)
        .and_then(|expression| extract_u32_literal(expression).ok())
        .unwrap_or(spec.default)
}

fn select_static_or_dynamic_title(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    dynamic_title_bindings: &[&str],
    static_title: &'static str,
    dynamic_title: &'static str,
) -> &'static str {
    if dynamic_title_bindings
        .iter()
        .any(|binding| bindings.contains_key(*binding))
    {
        dynamic_title
    } else {
        static_title
    }
}

fn collect_mapped_indexed_text_grid<T, F>(
    expressions: &[StaticSpannedExpression],
    row_count: u32,
    col_count: u32,
    subset: &str,
    expression_config: &IndexedStaticExpressionConfig<'_>,
    grid: IndexedTextGridSpec<'_>,
    mut map: F,
) -> Result<Vec<((u32, u32), T)>, String>
where
    F: FnMut(String) -> Option<T>,
{
    let body = require_top_level_function_body(expressions, subset, grid.function_name)?;
    let (row_start, row_end) = match grid.rows {
        IndexedTextGridRowRange::Fixed(row) => (row, row),
        IndexedTextGridRowRange::InclusiveFromOneToRowCount => (1, row_count),
    };
    let rows: Vec<u32> = (row_start..=row_end).collect();
    let columns: Vec<u32> = (1..=col_count).collect();
    let mut values = Vec::with_capacity(rows.len().saturating_mul(columns.len()));
    for row in rows {
        for column in columns.iter().copied() {
            let text =
                match eval_indexed_static_expression(body, row, column, None, expression_config)? {
                    IndexedStaticValue::Text(text) => text,
                    IndexedStaticValue::Number(number) => number.to_string(),
                    IndexedStaticValue::Bool(_) => {
                        return Err(format!(
                            "{subset} {} must resolve to text",
                            grid.function_name
                        ));
                    }
                };
            if let Some(value) = map(text) {
                values.push(((row, column), value));
            }
        }
    }
    Ok(values)
}

fn collect_mapped_indexed_text_grid_map<T, F>(
    expressions: &[StaticSpannedExpression],
    row_count: u32,
    col_count: u32,
    subset: &str,
    expression_config: &IndexedStaticExpressionConfig<'_>,
    grid: IndexedTextGridSpec<'_>,
    map: F,
) -> Result<BTreeMap<(u32, u32), T>, String>
where
    F: FnMut(String) -> Option<T>,
{
    Ok(collect_mapped_indexed_text_grid(
        expressions,
        row_count,
        col_count,
        subset,
        expression_config,
        grid,
        map,
    )?
    .into_iter()
    .collect())
}

fn collect_mapped_indexed_text_grid_values<T, F>(
    expressions: &[StaticSpannedExpression],
    row_count: u32,
    col_count: u32,
    subset: &str,
    expression_config: &IndexedStaticExpressionConfig<'_>,
    grid: IndexedTextGridSpec<'_>,
    map: F,
) -> Result<Vec<T>, String>
where
    F: FnMut(String) -> Option<T>,
{
    Ok(collect_mapped_indexed_text_grid(
        expressions,
        row_count,
        col_count,
        subset,
        expression_config,
        grid,
        map,
    )?
    .into_iter()
    .map(|(_, value)| value)
    .collect())
}

fn derive_mapped_indexed_text_grid_state<T, S, F, G>(
    expressions: &[StaticSpannedExpression],
    row_count: u32,
    col_count: u32,
    subset: &str,
    expression_config: &IndexedStaticExpressionConfig<'_>,
    grid: IndexedTextGridSpec<'_>,
    map: F,
    build_state: G,
) -> Result<(BTreeMap<(u32, u32), T>, S), String>
where
    F: FnMut(String) -> Option<T>,
    G: FnOnce(&BTreeMap<(u32, u32), T>) -> S,
{
    let mapped = collect_mapped_indexed_text_grid_map(
        expressions,
        row_count,
        col_count,
        subset,
        expression_config,
        grid,
        map,
    )?;
    let state = build_state(&mapped);
    Ok((mapped, state))
}

fn eval_indexed_static_expression(
    expression: &StaticSpannedExpression,
    row: u32,
    column: u32,
    piped: Option<IndexedStaticValue>,
    config: &IndexedStaticExpressionConfig<'_>,
) -> Result<IndexedStaticValue, String> {
    match &expression.node {
        StaticExpression::Literal(Literal::Number(number)) => {
            Ok(IndexedStaticValue::Number(*number as i64))
        }
        StaticExpression::Literal(Literal::Text(text))
        | StaticExpression::Literal(Literal::Tag(text)) => {
            Ok(IndexedStaticValue::Text(text.as_str().to_string()))
        }
        StaticExpression::TextLiteral { parts, .. } => {
            let mut out = String::new();
            for part in parts {
                match part {
                    TextPart::Text(text) => out.push_str(text.as_str()),
                    TextPart::Interpolation { .. } => {
                        return Err(indexed_static_expression_error(
                            config,
                            "does not support interpolated text",
                        ));
                    }
                }
            }
            Ok(IndexedStaticValue::Text(out))
        }
        StaticExpression::Alias(alias) => match alias {
            boon::parser::static_expression::Alias::WithoutPassed { parts, .. }
                if parts.len() == 1
                    && config
                        .row_aliases
                        .iter()
                        .any(|alias| parts[0].as_str() == *alias) =>
            {
                Ok(IndexedStaticValue::Number(row as i64))
            }
            boon::parser::static_expression::Alias::WithoutPassed { parts, .. }
                if parts.len() == 1
                    && config
                        .column_aliases
                        .iter()
                        .any(|alias| parts[0].as_str() == *alias) =>
            {
                Ok(IndexedStaticValue::Number(column as i64))
            }
            _ => Err(indexed_static_expression_error(
                config,
                "uses unsupported alias",
            )),
        },
        StaticExpression::Comparator(Comparator::Equal {
            operand_a,
            operand_b,
        }) => Ok(IndexedStaticValue::Bool(
            eval_indexed_static_expression(operand_a, row, column, None, config)?
                == eval_indexed_static_expression(operand_b, row, column, None, config)?,
        )),
        StaticExpression::Pipe { from, to } => {
            let input = eval_indexed_static_expression(from, row, column, None, config)?;
            eval_indexed_static_expression(to, row, column, Some(input), config)
        }
        StaticExpression::When { arms } => {
            let source = piped.ok_or_else(|| {
                indexed_static_expression_error(config, "requires WHEN to have pipe input")
            })?;
            for arm in arms {
                if indexed_static_pattern_matches(&arm.pattern, &source, config)? {
                    return eval_indexed_static_expression(&arm.body, row, column, None, config);
                }
            }
            Err(indexed_static_expression_error(
                config,
                "found no matching WHEN arm",
            ))
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, config.empty_text_function_path) && arguments.is_empty() =>
        {
            Ok(IndexedStaticValue::Text(String::new()))
        }
        _ => Err(indexed_static_expression_error(
            config,
            "uses unsupported expression",
        )),
    }
}

fn indexed_static_pattern_matches(
    pattern: &Pattern,
    value: &IndexedStaticValue,
    config: &IndexedStaticExpressionConfig<'_>,
) -> Result<bool, String> {
    Ok(match pattern {
        Pattern::WildCard => true,
        Pattern::Literal(Literal::Number(number)) => {
            matches!(value, IndexedStaticValue::Number(current) if *current == *number as i64)
        }
        Pattern::Literal(Literal::Text(text)) | Pattern::Literal(Literal::Tag(text)) => {
            match text.as_str() {
                "True" => matches!(value, IndexedStaticValue::Bool(true)),
                "False" => matches!(value, IndexedStaticValue::Bool(false)),
                other => matches!(value, IndexedStaticValue::Text(current) if current == other),
            }
        }
        _ => {
            return Err(indexed_static_expression_error(
                config,
                "uses unsupported WHEN pattern",
            ));
        }
    })
}

fn indexed_static_expression_error(
    config: &IndexedStaticExpressionConfig<'_>,
    detail: &str,
) -> String {
    format!("{} {} {detail}", config.subset, config.context_label)
}

fn require_top_level_binding_expr<'a>(
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    subset: &str,
    binding_name: &str,
) -> Result<&'a StaticSpannedExpression, String> {
    require_named_top_level_binding_expr(bindings, &format!("{subset} subset"), binding_name)
}

fn require_named_top_level_binding_expr<'a>(
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    context_label: &str,
    binding_name: &str,
) -> Result<&'a StaticSpannedExpression, String> {
    bindings
        .get(binding_name)
        .copied()
        .ok_or_else(|| format!("{context_label} requires top-level `{binding_name}`"))
}

fn first_text_literal(expression: &StaticSpannedExpression) -> Option<String> {
    match &expression.node {
        StaticExpression::Literal(Literal::Number(_)) => None,
        StaticExpression::Literal(Literal::Text(text))
        | StaticExpression::Literal(Literal::Tag(text)) => Some(text.as_str().to_string()),
        StaticExpression::TextLiteral { parts, .. } => {
            let mut out = String::new();
            for part in parts {
                match part {
                    TextPart::Text(text) => out.push_str(text.as_str()),
                    TextPart::Interpolation { .. } => return None,
                }
            }
            Some(out)
        }
        StaticExpression::Variable(variable) => first_text_literal(&variable.value),
        StaticExpression::List { items } | StaticExpression::Latest { inputs: items } => {
            items.iter().find_map(first_text_literal)
        }
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .find_map(|variable| first_text_literal(&variable.node.value)),
        StaticExpression::Map { entries } => entries
            .iter()
            .find_map(|entry| first_text_literal(&entry.value)),
        StaticExpression::Function { body, .. } => first_text_literal(body),
        StaticExpression::FunctionCall { arguments, .. } => arguments
            .iter()
            .filter_map(|argument| argument.node.value.as_ref())
            .find_map(first_text_literal),
        StaticExpression::Hold { body, .. } | StaticExpression::Then { body } => {
            first_text_literal(body)
        }
        StaticExpression::Flush { value } | StaticExpression::Spread { value } => {
            first_text_literal(value)
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => {
            arms.iter().find_map(|arm| first_text_literal(&arm.body))
        }
        StaticExpression::Pipe { from, to } => {
            first_text_literal(from).or_else(|| first_text_literal(to))
        }
        StaticExpression::Block { variables, output } => variables
            .iter()
            .find_map(|variable| first_text_literal(&variable.node.value))
            .or_else(|| first_text_literal(output)),
        StaticExpression::Comparator(comparator) => comparator_operands(comparator)
            .into_iter()
            .find_map(first_text_literal),
        StaticExpression::ArithmeticOperator(operator) => arithmetic_operands(operator)
            .into_iter()
            .find_map(first_text_literal),
        StaticExpression::Bits { size } | StaticExpression::Memory { address: size } => {
            first_text_literal(size)
        }
        StaticExpression::Bytes { data } => data.iter().find_map(first_text_literal),
        StaticExpression::PostfixFieldAccess { expr, .. } => first_text_literal(expr),
        StaticExpression::Alias(_)
        | StaticExpression::LinkSetter { .. }
        | StaticExpression::Link
        | StaticExpression::Skip
        | StaticExpression::FieldAccess { .. } => None,
    }
}

fn require_top_level_function_body<'a>(
    expressions: &'a [StaticSpannedExpression],
    subset: &str,
    function_name: &str,
) -> Result<&'a StaticSpannedExpression, String> {
    let function = require_top_level_function_expr(expressions, subset, function_name)?;
    let StaticExpression::Function { body, .. } = &function.node else {
        unreachable!("top-level function lookup returned non-function expression");
    };
    Ok(body.as_ref())
}

fn require_top_level_function_expr<'a>(
    expressions: &'a [StaticSpannedExpression],
    subset: &str,
    function_name: &str,
) -> Result<&'a StaticSpannedExpression, String> {
    expressions
        .iter()
        .find(|expression| {
            matches!(
                &expression.node,
                StaticExpression::Function { name, .. } if name.as_str() == function_name
            )
        })
        .ok_or_else(|| format!("{subset} subset requires top-level function `{function_name}`"))
}

fn extract_optional_text_from_top_level_binding<'a>(
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    subset: &str,
    binding_name: &str,
    binding_path: &[&str],
    traversal_steps: &[StaticExpressionTraverseStep<'_>],
) -> Result<Option<String>, String> {
    let binding = require_top_level_binding_expr(bindings, subset, binding_name)?;
    let StaticExpression::FunctionCall { path, .. } = &binding.node else {
        return Err(format!(
            "{subset} subset requires top-level `{binding_name}` to call `{}`",
            binding_path.join("/")
        ));
    };
    if !path_matches(path, binding_path) {
        return Err(format!(
            "{subset} subset requires top-level `{binding_name}` to call `{}`",
            binding_path.join("/")
        ));
    }
    Ok(traversal_steps
        .iter()
        .try_fold(binding, |current, step| match step {
            StaticExpressionTraverseStep::FunctionArgument {
                path: expected_path,
                argument_name,
            } => {
                let StaticExpression::FunctionCall { path, arguments } = &current.node else {
                    return None;
                };
                if !path_matches(path, expected_path) {
                    return None;
                }
                arguments
                    .iter()
                    .find(|argument| argument.node.name.as_str() == *argument_name)
                    .and_then(|argument| argument.node.value.as_ref())
            }
            StaticExpressionTraverseStep::ListItem(index) => match &current.node {
                StaticExpression::List { items } => items.get(*index),
                _ => None,
            },
        })
        .and_then(first_text_literal))
}

fn comparator_operands(comparator: &Comparator) -> Vec<&StaticSpannedExpression> {
    match comparator {
        Comparator::Equal {
            operand_a,
            operand_b,
        }
        | Comparator::NotEqual {
            operand_a,
            operand_b,
        }
        | Comparator::Greater {
            operand_a,
            operand_b,
        }
        | Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        }
        | Comparator::Less {
            operand_a,
            operand_b,
        }
        | Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => vec![operand_a, operand_b],
    }
}

fn arithmetic_operands(
    operator: &boon::parser::static_expression::ArithmeticOperator,
) -> Vec<&StaticSpannedExpression> {
    match operator {
        boon::parser::static_expression::ArithmeticOperator::Negate { operand } => vec![operand],
        boon::parser::static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }
        | boon::parser::static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }
        | boon::parser::static_expression::ArithmeticOperator::Multiply {
            operand_a,
            operand_b,
        }
        | boon::parser::static_expression::ArithmeticOperator::Divide {
            operand_a,
            operand_b,
        } => vec![operand_a, operand_b],
    }
}

fn indexed_grid_root_node(
    program: &CellsProgram,
    sheet: &CellsSheetState,
    editing: Option<CellsEditingView<'_>>,
) -> HostViewNode {
    let header = generic_host_node(
        ViewSiteId(402),
        FunctionInstanceId(10),
        None,
        HostViewKind::GenericElement {
            tag: "div".to_string(),
            text: Some(program.display_title.clone()),
            properties: vec![("tabindex".to_string(), Some("0".to_string()))],
            styles: Vec::new(),
            input_value: None,
            checked: None,
            event_bindings: vec![HostElementEventBinding {
                source_port: CellsProgram::HEADING_CLICK_PORT,
                event_kind: UiEventKind::Click,
            }],
        },
        Vec::new(),
    );

    let mut header_children = vec![generic_host_node(
        ViewSiteId(403),
        FunctionInstanceId(10),
        None,
        HostViewKind::GenericElement {
            tag: "span".to_string(),
            text: Some(String::new()),
            properties: Vec::new(),
            styles: Vec::new(),
            input_value: None,
            checked: None,
            event_bindings: Vec::new(),
        },
        Vec::new(),
    )];
    for (index, header_text) in program.column_headers.iter().enumerate() {
        header_children.push(generic_host_node(
            ViewSiteId(405 + index as u32),
            FunctionInstanceId(10),
            None,
            HostViewKind::GenericElement {
                tag: "span".to_string(),
                text: Some(header_text.clone()),
                properties: Vec::new(),
                styles: Vec::new(),
                input_value: None,
                checked: None,
                event_bindings: Vec::new(),
            },
            Vec::new(),
        ));
    }
    let header_row = generic_host_node(
        ViewSiteId(430),
        FunctionInstanceId(10),
        None,
        HostViewKind::GenericElement {
            tag: "div".to_string(),
            text: None,
            properties: Vec::new(),
            styles: Vec::new(),
            input_value: None,
            checked: None,
            event_bindings: Vec::new(),
        },
        header_children,
    );

    let mut row_nodes = Vec::with_capacity(program.row_count as usize);
    for row in 1..=program.row_count {
        let mut row_children = vec![generic_host_node(
            ViewSiteId(431),
            FunctionInstanceId(11),
            Some(row as u64),
            HostViewKind::GenericElement {
                tag: "span".to_string(),
                text: Some(row.to_string()),
                properties: Vec::new(),
                styles: Vec::new(),
                input_value: None,
                checked: None,
                event_bindings: Vec::new(),
            },
            Vec::new(),
        )];

        for column in 1..=program.col_count {
            let mapped_item_identity = indexed_grid_cell_identity(row, column);
            let is_editing =
                editing.is_some_and(|state| state.row == row && state.column == column);
            if is_editing {
                let editing = editing.expect("checked above");
                let mut properties = vec![
                    (
                        "data-boon-link-path".to_string(),
                        Some(indexed_grid_cell_edit_link_path(row, column)),
                    ),
                    ("type".to_string(), Some("text".to_string())),
                ];
                if editing.focus_hint {
                    properties.push(("autofocus".to_string(), Some("true".to_string())));
                    properties.push(("focused".to_string(), Some("true".to_string())));
                }
                row_children.push(generic_host_node(
                    ViewSiteId(433),
                    FunctionInstanceId(11),
                    Some(mapped_item_identity),
                    HostViewKind::GenericElement {
                        tag: "input".to_string(),
                        text: None,
                        properties,
                        styles: vec![
                            ("width".to_string(), Some("80px".to_string())),
                            ("height".to_string(), Some("26px".to_string())),
                        ],
                        input_value: Some(editing.draft.to_string()),
                        checked: None,
                        event_bindings: vec![
                            HostElementEventBinding {
                                source_port: indexed_grid_cell_edit_port(row, column, 1),
                                event_kind: UiEventKind::Input,
                            },
                            HostElementEventBinding {
                                source_port: indexed_grid_cell_edit_port(row, column, 2),
                                event_kind: UiEventKind::KeyDown,
                            },
                            HostElementEventBinding {
                                source_port: indexed_grid_cell_edit_port(row, column, 3),
                                event_kind: UiEventKind::Blur,
                            },
                        ],
                    },
                    Vec::new(),
                ));
            } else {
                row_children.push(generic_host_node(
                    ViewSiteId(434),
                    FunctionInstanceId(11),
                    Some(mapped_item_identity),
                    HostViewKind::GenericElement {
                        tag: "span".to_string(),
                        text: Some(sheet.display_text(row, column)),
                        properties: vec![(
                            "data-boon-link-path".to_string(),
                            Some(indexed_grid_cell_display_link_path(row, column)),
                        )],
                        styles: vec![
                            ("display".to_string(), Some("inline-block".to_string())),
                            ("width".to_string(), Some("80px".to_string())),
                            ("height".to_string(), Some("26px".to_string())),
                            ("padding-left".to_string(), Some("8px".to_string())),
                        ],
                        input_value: None,
                        checked: None,
                        event_bindings: vec![
                            HostElementEventBinding {
                                source_port: indexed_grid_cell_click_port(row, column),
                                event_kind: UiEventKind::Click,
                            },
                            HostElementEventBinding {
                                source_port: indexed_grid_cell_display_port(row, column),
                                event_kind: UiEventKind::DoubleClick,
                            },
                        ],
                    },
                    Vec::new(),
                ));
            }
        }

        row_nodes.push(generic_host_node(
            ViewSiteId(432),
            FunctionInstanceId(11),
            Some(row as u64),
            HostViewKind::GenericElement {
                tag: "div".to_string(),
                text: None,
                properties: vec![(
                    "data-boon-link-path".to_string(),
                    Some(indexed_grid_row_link_path(row)),
                )],
                styles: Vec::new(),
                input_value: None,
                checked: None,
                event_bindings: Vec::new(),
            },
            row_children,
        ));
    }

    let body = generic_host_node(
        ViewSiteId(400),
        FunctionInstanceId(10),
        None,
        HostViewKind::GenericElement {
            tag: "div".to_string(),
            text: None,
            properties: Vec::new(),
            styles: vec![
                ("height".to_string(), Some("500px".to_string())),
                ("overflow".to_string(), Some("auto".to_string())),
            ],
            input_value: None,
            checked: None,
            event_bindings: Vec::new(),
        },
        row_nodes,
    );

    generic_host_node(
        ViewSiteId(401),
        FunctionInstanceId(10),
        None,
        HostViewKind::GenericElement {
            tag: "div".to_string(),
            text: None,
            properties: Vec::new(),
            styles: Vec::new(),
            input_value: None,
            checked: None,
            event_bindings: Vec::new(),
        },
        vec![header, header_row, body],
    )
}

fn generic_host_node(
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    mapped_item_identity: Option<u64>,
    kind: HostViewKind,
    children: Vec<HostViewNode>,
) -> HostViewNode {
    HostViewNode {
        retained_key: RetainedNodeKey {
            view_site,
            function_instance: Some(function_instance),
            mapped_item_identity,
        },
        kind,
        children,
    }
}

fn indexed_grid_cell_identity(row: u32, column: u32) -> u64 {
    (row as u64) * 1_000 + column as u64
}

fn indexed_grid_row_link_path(row: u32) -> String {
    format!("all_row_cells.{:04}.element", row.saturating_sub(1))
}

fn indexed_grid_cell_display_link_path(row: u32, column: u32) -> String {
    format!(
        "all_row_cells.{:04}.cells.{:04}.display_element",
        row.saturating_sub(1),
        column.saturating_sub(1)
    )
}

fn indexed_grid_cell_edit_link_path(row: u32, column: u32) -> String {
    format!(
        "all_row_cells.{:04}.cells.{:04}.editing_element",
        row.saturating_sub(1),
        column.saturating_sub(1)
    )
}

fn indexed_grid_cell_display_port(row: u32, column: u32) -> SourcePortId {
    SourcePortId(10_000 + row * 100 + column)
}

fn indexed_grid_cell_click_port(row: u32, column: u32) -> SourcePortId {
    SourcePortId(100_000 + row * 100 + column)
}

fn indexed_grid_cell_edit_port(row: u32, column: u32, suffix: u32) -> SourcePortId {
    SourcePortId(200_000 + row * 1_000 + column * 10 + suffix)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoSelectedFilter {
    All,
    Active,
    Completed,
}

fn derive_editable_filterable_list_host_view_sink_values(
    base_sink_values: &BTreeMap<SinkPortId, KernelValue>,
) -> BTreeMap<SinkPortId, KernelValue> {
    let todos = editable_filterable_list_items(base_sink_values);
    let active_count = editable_filterable_list_active_count(base_sink_values, &todos);
    let mut sink_values = base_sink_values.clone();
    sink_values.insert(
        TodoProgram::VISIBLE_TODOS_SINK,
        KernelValue::List(editable_filterable_list_visible_items(
            &todos,
            editable_filterable_list_selected_filter(base_sink_values),
        )),
    );
    sink_values.insert(
        TodoProgram::ACTIVE_COUNT_LABEL_SINK,
        KernelValue::from(if active_count == 1 {
            "1 item left".to_string()
        } else {
            format!("{active_count} items left")
        }),
    );
    // Set outline values for filter buttons based on selected filter
    let selected_filter = editable_filterable_list_selected_filter(base_sink_values);
    let outline_all = if matches!(selected_filter, TodoSelectedFilter::All) {
        "2px solid rgba(148, 163, 184, 0.9)"
    } else {
        "none"
    };
    let outline_active = if matches!(selected_filter, TodoSelectedFilter::Active) {
        "2px solid rgba(148, 163, 184, 0.9)"
    } else {
        "none"
    };
    let outline_completed = if matches!(selected_filter, TodoSelectedFilter::Completed) {
        "2px solid rgba(148, 163, 184, 0.9)"
    } else {
        "none"
    };
    sink_values.insert(
        TodoProgram::FILTER_ALL_OUTLINE_SINK,
        KernelValue::from(outline_all),
    );
    sink_values.insert(
        TodoProgram::FILTER_ACTIVE_OUTLINE_SINK,
        KernelValue::from(outline_active),
    );
    sink_values.insert(
        TodoProgram::FILTER_COMPLETED_OUTLINE_SINK,
        KernelValue::from(outline_completed),
    );
    sink_values
}

fn materialize_host_view_from_derived_sink_values(
    base_sink_values: &BTreeMap<SinkPortId, KernelValue>,
    derive_sink_values: impl FnOnce(
        &BTreeMap<SinkPortId, KernelValue>,
    ) -> BTreeMap<SinkPortId, KernelValue>,
    template: HostViewTemplate,
) -> Result<HostViewIr, String> {
    let sink_values = derive_sink_values(base_sink_values);
    materialize_host_view_template(&template, &sink_values)
}

struct ExecutorDerivedHostViewProgramConfig {
    derive_sink_values: fn(&BTreeMap<SinkPortId, KernelValue>) -> BTreeMap<SinkPortId, KernelValue>,
    host_view_template: fn() -> HostViewTemplate,
    program: IrHostViewLoweredProgramSpec,
}

fn build_persistent_executor_derived_host_view_output(
    _expressions: &[StaticSpannedExpression],
    _bindings: &BTreeMap<String, &StaticSpannedExpression>,
    ir: IrProgram,
    config: &ExecutorDerivedHostViewProgramConfig,
) -> Result<LoweredProgram, String> {
    let executor = IrExecutor::new(ir.clone())?;
    let host_view = materialize_host_view_from_derived_sink_values(
        &executor.sink_values(),
        config.derive_sink_values,
        (config.host_view_template)(),
    )?;
    Ok(build_ir_host_view_lowered_program(
        ir,
        host_view,
        config.program,
    ))
}

fn editable_filterable_list_items(
    base_sink_values: &BTreeMap<SinkPortId, KernelValue>,
) -> Vec<KernelValue> {
    match base_sink_values.get(&TodoProgram::TODOS_LIST_SINK) {
        Some(KernelValue::List(items)) => items.clone(),
        _ => Vec::new(),
    }
}

fn editable_filterable_list_active_count(
    base_sink_values: &BTreeMap<SinkPortId, KernelValue>,
    todos: &[KernelValue],
) -> usize {
    match base_sink_values.get(&TodoProgram::ACTIVE_COUNT_SINK) {
        Some(KernelValue::Number(number)) if *number >= 0.0 => *number as usize,
        _ => todos
            .iter()
            .filter(|todo| !editable_filterable_list_item_completed(todo))
            .count(),
    }
}

fn editable_filterable_list_selected_filter(
    base_sink_values: &BTreeMap<SinkPortId, KernelValue>,
) -> TodoSelectedFilter {
    match base_sink_values.get(&TodoProgram::SELECTED_FILTER_SINK) {
        Some(KernelValue::Text(text)) | Some(KernelValue::Tag(text))
            if text == "active" || text == "Active" =>
        {
            TodoSelectedFilter::Active
        }
        Some(KernelValue::Text(text)) | Some(KernelValue::Tag(text))
            if text == "completed" || text == "Completed" =>
        {
            TodoSelectedFilter::Completed
        }
        _ => TodoSelectedFilter::All,
    }
}

fn editable_filterable_list_visible_items(
    todos: &[KernelValue],
    selected_filter: TodoSelectedFilter,
) -> Vec<KernelValue> {
    todos
        .iter()
        .filter(|todo| editable_filterable_list_matches_filter(todo, selected_filter))
        .cloned()
        .collect()
}

fn editable_filterable_list_matches_filter(
    todo: &KernelValue,
    selected_filter: TodoSelectedFilter,
) -> bool {
    match selected_filter {
        TodoSelectedFilter::All => true,
        TodoSelectedFilter::Active => !editable_filterable_list_item_completed(todo),
        TodoSelectedFilter::Completed => editable_filterable_list_item_completed(todo),
    }
}

fn editable_filterable_list_item_completed(todo: &KernelValue) -> bool {
    match todo {
        KernelValue::Object(fields) => {
            matches!(fields.get("completed"), Some(KernelValue::Bool(true)))
        }
        _ => false,
    }
}

fn editable_filterable_list_host_view_template() -> HostViewTemplate {
    HostViewTemplate::Node(HostViewTemplateNode {
        view_site: ViewSiteId(200),
        function_instance: FunctionInstanceId(1),
        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Document),
        children: vec![HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(201),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StripeLayout {
                direction: HostStripeDirection::Column,
                gap_px: 8,
                padding_px: Some(0),
                width: None,
                align_cross: None,
            }),
            children: {
                let mut children = vec![
                    editable_filterable_list_header_node(),
                    editable_filterable_list_input_row_node(),
                ];
                children.push(HostViewTemplate::Conditional {
                    condition: HostViewTemplateCondition::ListNotEmpty(
                        TodoProgram::TODOS_LIST_SINK,
                    ),
                    when_true: vec![
                        editable_filterable_list_list_node(),
                        editable_filterable_list_footer_node(),
                    ],
                    when_false: Vec::new(),
                });
                children.extend(editable_filterable_list_footer_text_nodes());
                children
            },
        })],
    })
}

fn editable_filterable_list_header_node() -> HostViewTemplate {
    HostViewTemplate::Node(HostViewTemplateNode {
        view_site: ViewSiteId(202),
        function_instance: FunctionInstanceId(1),
        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StaticLabel {
            text: "todos".to_string(),
        }),
        children: Vec::new(),
    })
}

fn editable_filterable_list_input_row_node() -> HostViewTemplate {
    let mut children = vec![HostViewTemplate::Conditional {
        condition: HostViewTemplateCondition::ListNotEmpty(TodoProgram::TODOS_LIST_SINK),
        when_true: vec![HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(204),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::BoundCheckbox {
                checked: HostViewTemplateValue::Sink(TodoProgram::ALL_COMPLETED_SINK),
                click_port: TodoProgram::TOGGLE_ALL_PORT,
                labelled_by_view_site: None,
            },
            children: Vec::new(),
        })],
        when_false: Vec::new(),
    }];
    children.push(HostViewTemplate::Node(HostViewTemplateNode {
        view_site: ViewSiteId(205),
        function_instance: FunctionInstanceId(1),
        kind: HostViewTemplateNodeKind::BoundTextInput {
            value: HostViewTemplateValue::Sink(TodoProgram::MAIN_INPUT_TEXT_SINK),
            placeholder: "What needs to be done?".to_string(),
            change_port: TodoProgram::MAIN_INPUT_CHANGE_PORT,
            key_down_port: TodoProgram::MAIN_INPUT_KEY_DOWN_PORT,
            blur_port: Some(TodoProgram::MAIN_INPUT_BLUR_PORT),
            focus_port: Some(TodoProgram::MAIN_INPUT_FOCUS_PORT),
            focus_on_mount: HostViewTemplateCondition::SinkTruthy(
                TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK,
            ),
        },
        children: Vec::new(),
    }));

    HostViewTemplate::Node(HostViewTemplateNode {
        view_site: ViewSiteId(203),
        function_instance: FunctionInstanceId(1),
        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StripeLayout {
            direction: HostStripeDirection::Row,
            gap_px: 8,
            padding_px: Some(0),
            width: None,
            align_cross: None,
        }),
        children,
    })
}

fn editable_filterable_list_list_node() -> HostViewTemplate {
    HostViewTemplate::Node(HostViewTemplateNode {
        view_site: ViewSiteId(206),
        function_instance: FunctionInstanceId(1),
        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StripeLayout {
            direction: HostStripeDirection::Column,
            gap_px: 4,
            padding_px: Some(0),
            width: None,
            align_cross: None,
        }),
        children: vec![HostViewTemplate::Repeat {
            list_sink: TodoProgram::VISIBLE_TODOS_SINK,
            item_identity_field: "id",
            body: vec![editable_filterable_list_row_node()],
        }],
    })
}

fn editable_filterable_list_row_node() -> HostViewTemplate {
    HostViewTemplate::Node(HostViewTemplateNode {
        view_site: ViewSiteId(300),
        function_instance: FunctionInstanceId(2),
        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StripeLayout {
            direction: HostStripeDirection::Row,
            gap_px: 8,
            padding_px: Some(0),
            width: None,
            align_cross: None,
        }),
        children: vec![
            HostViewTemplate::Node(HostViewTemplateNode {
                view_site: ViewSiteId(301),
                function_instance: FunctionInstanceId(2),
                kind: HostViewTemplateNodeKind::BoundCheckbox {
                    checked: HostViewTemplateValue::ItemField("completed"),
                    click_port: TodoProgram::TODO_TOGGLE_PORT,
                    labelled_by_view_site: Some(ViewSiteId(302)),
                },
                children: Vec::new(),
            }),
            HostViewTemplate::Conditional {
                condition: HostViewTemplateCondition::ItemIdentityEqualsSink(
                    TodoProgram::EDIT_TARGET_SINK,
                ),
                when_true: vec![HostViewTemplate::Node(HostViewTemplateNode {
                    view_site: ViewSiteId(303),
                    function_instance: FunctionInstanceId(2),
                    kind: HostViewTemplateNodeKind::BoundTextInput {
                        value: HostViewTemplateValue::Sink(TodoProgram::EDIT_DRAFT_SINK),
                        placeholder: "Edit todo".to_string(),
                        change_port: TodoProgram::TODO_EDIT_CHANGE_PORT,
                        key_down_port: TodoProgram::TODO_EDIT_COMMIT_PORT,
                        blur_port: Some(TodoProgram::TODO_EDIT_BLUR_PORT),
                        focus_port: Some(TodoProgram::TODO_EDIT_FOCUS_PORT),
                        focus_on_mount: HostViewTemplateCondition::SinkTruthy(
                            TodoProgram::EDIT_FOCUS_HINT_SINK,
                        ),
                    },
                    children: Vec::new(),
                })],
                when_false: vec![HostViewTemplate::Node(HostViewTemplateNode {
                    view_site: ViewSiteId(302),
                    function_instance: FunctionInstanceId(2),
                    kind: HostViewTemplateNodeKind::BoundActionLabel {
                        value: HostViewTemplateValue::ItemField("title"),
                        press_port: TodoProgram::TODO_BEGIN_EDIT_PORT,
                        event_kind: UiEventKind::DoubleClick,
                    },
                    children: Vec::new(),
                })],
            },
            HostViewTemplate::Conditional {
                condition: HostViewTemplateCondition::ItemIdentityEqualsSink(
                    TodoProgram::HOVERED_TARGET_SINK,
                ),
                when_true: vec![HostViewTemplate::Node(HostViewTemplateNode {
                    view_site: ViewSiteId(304),
                    function_instance: FunctionInstanceId(2),
                    kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Button {
                        label: HostButtonLabel::Static("×".to_string()),
                        press_port: TodoProgram::TODO_DELETE_PORT,
                        disabled_sink: None,
                    }),
                    children: Vec::new(),
                })],
                when_false: Vec::new(),
            },
        ],
    })
}

fn editable_filterable_list_footer_node() -> HostViewTemplate {
    let mut children = vec![
        HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(207),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::BoundLabel {
                value: HostViewTemplateValue::Sink(TodoProgram::ACTIVE_COUNT_LABEL_SINK),
            },
            children: Vec::new(),
        }),
        HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(208),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StyledButton {
                label: HostButtonLabel::Static("All".to_string()),
                press_port: TodoProgram::FILTER_ALL_PORT,
                disabled_sink: None,
                width: None,
                padding_px: Some(4),
                rounded_fully: true,
                background: None,
                background_sink: None,
                active_background: None,
                outline_sink: None,
                active_outline: Some("2px solid rgba(148, 163, 184, 0.9)".to_string()),
            }),
            children: Vec::new(),
        }),
        HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(209),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StyledButton {
                label: HostButtonLabel::Static("Active".to_string()),
                press_port: TodoProgram::FILTER_ACTIVE_PORT,
                disabled_sink: None,
                width: None,
                padding_px: Some(4),
                rounded_fully: true,
                background: None,
                background_sink: None,
                active_background: None,
                outline_sink: Some(TodoProgram::FILTER_ACTIVE_OUTLINE_SINK),
                active_outline: None,
            }),
            children: Vec::new(),
        }),
        HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(210),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StyledButton {
                label: HostButtonLabel::Static("Completed".to_string()),
                press_port: TodoProgram::FILTER_COMPLETED_PORT,
                disabled_sink: None,
                width: None,
                padding_px: Some(4),
                rounded_fully: true,
                background: None,
                background_sink: None,
                active_background: None,
                outline_sink: Some(TodoProgram::FILTER_COMPLETED_OUTLINE_SINK),
                active_outline: None,
            }),
            children: Vec::new(),
        }),
        HostViewTemplate::Conditional {
            condition: HostViewTemplateCondition::SinkGreaterThanZero(
                TodoProgram::COMPLETED_COUNT_SINK,
            ),
            when_true: vec![HostViewTemplate::Node(HostViewTemplateNode {
                view_site: ViewSiteId(211),
                function_instance: FunctionInstanceId(1),
                kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Button {
                    label: HostButtonLabel::Static("Clear completed".to_string()),
                    press_port: TodoProgram::CLEAR_COMPLETED_PORT,
                    disabled_sink: None,
                }),
                children: Vec::new(),
            })],
            when_false: Vec::new(),
        },
    ];
    HostViewTemplate::Node(HostViewTemplateNode {
        view_site: ViewSiteId(212),
        function_instance: FunctionInstanceId(1),
        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StripeLayout {
            direction: HostStripeDirection::Row,
            gap_px: 8,
            padding_px: Some(0),
            width: None,
            align_cross: None,
        }),
        children: {
            let mut inner = Vec::new();
            inner.append(&mut children);
            inner
        },
    })
}

fn editable_filterable_list_footer_text_nodes() -> Vec<HostViewTemplate> {
    vec![
        HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(214),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Paragraph),
            children: vec![HostViewTemplate::Node(HostViewTemplateNode {
                view_site: ViewSiteId(2141),
                function_instance: FunctionInstanceId(1),
                kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StaticLabel {
                    text: "Double-click to edit a todo".to_string(),
                }),
                children: Vec::new(),
            })],
        }),
        HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(215),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Paragraph),
            children: vec![
                HostViewTemplate::Node(HostViewTemplateNode {
                    view_site: ViewSiteId(2151),
                    function_instance: FunctionInstanceId(1),
                    kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StaticLabel {
                        text: "Created by ".to_string(),
                    }),
                    children: Vec::new(),
                }),
                HostViewTemplate::Node(HostViewTemplateNode {
                    view_site: ViewSiteId(2152),
                    function_instance: FunctionInstanceId(1),
                    kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Link {
                        href: "https://kavik.cz/".to_string(),
                        new_tab: true,
                    }),
                    children: vec![HostViewTemplate::Node(HostViewTemplateNode {
                        view_site: ViewSiteId(2153),
                        function_instance: FunctionInstanceId(1),
                        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StaticLabel {
                            text: "Martin Kavík".to_string(),
                        }),
                        children: Vec::new(),
                    })],
                }),
            ],
        }),
        HostViewTemplate::Node(HostViewTemplateNode {
            view_site: ViewSiteId(216),
            function_instance: FunctionInstanceId(1),
            kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Paragraph),
            children: vec![
                HostViewTemplate::Node(HostViewTemplateNode {
                    view_site: ViewSiteId(2161),
                    function_instance: FunctionInstanceId(1),
                    kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StaticLabel {
                        text: "Part of ".to_string(),
                    }),
                    children: Vec::new(),
                }),
                HostViewTemplate::Node(HostViewTemplateNode {
                    view_site: ViewSiteId(2162),
                    function_instance: FunctionInstanceId(1),
                    kind: HostViewTemplateNodeKind::Concrete(HostViewKind::Link {
                        href: "http://todomvc.com".to_string(),
                        new_tab: true,
                    }),
                    children: vec![HostViewTemplate::Node(HostViewTemplateNode {
                        view_site: ViewSiteId(2163),
                        function_instance: FunctionInstanceId(1),
                        kind: HostViewTemplateNodeKind::Concrete(HostViewKind::StaticLabel {
                            text: "TodoMVC".to_string(),
                        }),
                        children: Vec::new(),
                    })],
                }),
            ],
        }),
    ]
}

fn lower_editable_filterable_list_ui_state_ir(
    todos_persistence: Vec<IrNodePersistence>,
) -> IrProgram {
    let mut nodes = Vec::new();
    append_literal(&mut nodes, KernelValue::from("all"), 1400);
    append_source_triggered_literal_hold_sink(
        &mut nodes,
        NodeId(1400),
        &[
            SourceTriggeredLiteralConfig {
                source_port: TodoProgram::FILTER_ALL_PORT,
                literal: KernelValue::from("all"),
                source_node_id: 1401,
                literal_node_id: 1402,
                then_node_id: 1403,
            },
            SourceTriggeredLiteralConfig {
                source_port: TodoProgram::FILTER_ACTIVE_PORT,
                literal: KernelValue::from("active"),
                source_node_id: 1404,
                literal_node_id: 1405,
                then_node_id: 1406,
            },
            SourceTriggeredLiteralConfig {
                source_port: TodoProgram::FILTER_COMPLETED_PORT,
                literal: KernelValue::from("completed"),
                source_node_id: 1407,
                literal_node_id: 1408,
                then_node_id: 1409,
            },
        ],
        1410,
        1411,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::SELECTED_FILTER_SINK,
        1412,
    );
    append_literal(&mut nodes, KernelValue::from(""), 1420);
    append_mirror_cell(&mut nodes, TodoProgram::MAIN_INPUT_DRAFT_CELL, 1421);
    append_source_port(&mut nodes, TodoProgram::MAIN_INPUT_CHANGE_PORT, 1422);
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1420),
        vec![NodeId(1421), NodeId(1422)],
        &[TriggeredUpdateConfig {
            source: NodeId(1444),
            body: NodeId(1434),
            then_node_id: 1561,
        }],
        1423,
        1424,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::MAIN_INPUT_TEXT_SINK,
        1425,
    );
    append_literal(&mut nodes, KernelValue::from(true), 1426);
    append_mirror_cell(&mut nodes, TodoProgram::MAIN_INPUT_FOCUSED_CELL, 1427);
    append_mirror_cell(&mut nodes, TodoProgram::NEXT_TODO_ID_CELL, 1431);
    append_literal(&mut nodes, KernelValue::from(false), 1432);
    nodes.push(IrNode {
        id: NodeId(1433),
        source_expr: None,
        kind: IrNodeKind::Skip,
    });
    append_literal(&mut nodes, KernelValue::from(""), 1434);
    append_source_port(&mut nodes, TodoProgram::MAIN_INPUT_KEY_DOWN_PORT, 1440);
    let todo_hover = append_source_port(&mut nodes, TodoProgram::TODO_HOVER_PORT, 1556);
    append_field_read(&mut nodes, todo_hover, "id", 1445);
    let todo_toggle = append_source_port(&mut nodes, TodoProgram::TODO_TOGGLE_PORT, 1446);
    append_field_read(&mut nodes, todo_toggle, "id", 1553);
    append_source_port(&mut nodes, TodoProgram::TOGGLE_ALL_PORT, 1452);
    append_source_port(&mut nodes, TodoProgram::CLEAR_COMPLETED_PORT, 1455);
    append_source_port(&mut nodes, TodoProgram::TODO_EDIT_CHANGE_PORT, 1458);
    append_key_down_match(
        &mut nodes,
        NodeId(1440),
        "Enter",
        NodeId(1439),
        NodeId(1433),
        1441,
        1444,
    );
    append_list_all_object_bool_field(&mut nodes, NodeId(1430), "completed", 1449);
    nodes.extend([
        IrNode {
            id: NodeId(1447),
            source_expr: None,
            kind: IrNodeKind::ListMapToggleObjectBoolFieldByFieldEq {
                list: NodeId(1430),
                match_field: "id".to_string(),
                match_value: NodeId(1553),
                bool_field: "completed".to_string(),
            },
        },
        IrNode {
            id: NodeId(1451),
            source_expr: None,
            kind: IrNodeKind::ListMapObjectBoolField {
                list: NodeId(1430),
                field: "completed".to_string(),
                value: NodeId(1450),
            },
        },
        IrNode {
            id: NodeId(1454),
            source_expr: None,
            kind: IrNodeKind::ListRetainObjectBoolField {
                list: NodeId(1430),
                field: "completed".to_string(),
                keep_if: false,
            },
        },
    ]);
    append_mirror_cell(&mut nodes, TodoProgram::EDIT_TITLE_CELL, 1457);
    append_bool_not(&mut nodes, NodeId(1449), 1450);
    let todo_edit_commit = append_source_port(&mut nodes, TodoProgram::TODO_EDIT_COMMIT_PORT, 1464);
    append_field_read(&mut nodes, todo_edit_commit, "id", 1554);
    append_field_read(&mut nodes, todo_edit_commit, "title", 1557);
    let trimmed_new_todo_title = append_trimmed_text(&mut nodes, NodeId(1424), 1435);
    nodes.push(IrNode {
        id: NodeId(1437),
        source_expr: None,
        kind: IrNodeKind::ObjectLiteral {
            fields: vec![
                ("id".to_string(), NodeId(1431)),
                ("title".to_string(), trimmed_new_todo_title),
                ("completed".to_string(), NodeId(1432)),
            ],
        },
    });
    let gated_new_todo = append_non_empty_value_or_skip(
        &mut nodes,
        trimmed_new_todo_title,
        NodeId(1434),
        NodeId(1437),
        1436,
        1438,
        NodeId(1433),
    );
    nodes.push(IrNode {
        id: NodeId(1439),
        source_expr: None,
        kind: IrNodeKind::ListAppend {
            list: NodeId(1430),
            item: gated_new_todo,
        },
    });
    append_latest_inputs(
        &mut nodes,
        vec![NodeId(1457), NodeId(1458), NodeId(1557)],
        1459,
        LatestHoldMode::AlwaysCreateLatest,
    );
    let trimmed_edit_title = append_trimmed_text(&mut nodes, NodeId(1459), 1460);
    let todo_delete = append_source_port(&mut nodes, TodoProgram::TODO_DELETE_PORT, 1467);
    append_field_read(&mut nodes, todo_delete, "id", 1555);
    nodes.extend([
        IrNode {
            id: NodeId(1462),
            source_expr: None,
            kind: IrNodeKind::ListMapObjectFieldByFieldEq {
                list: NodeId(1430),
                match_field: "id".to_string(),
                match_value: NodeId(1554),
                update_field: "title".to_string(),
                update_value: trimmed_edit_title,
            },
        },
        IrNode {
            id: NodeId(1466),
            source_expr: None,
            kind: IrNodeKind::ListRemoveObjectByFieldEq {
                list: NodeId(1430),
                field: "id".to_string(),
                value: NodeId(1555),
            },
        },
    ]);
    append_literal(&mut nodes, KernelValue::Tag("none".to_string()), 1472);
    append_source_port(&mut nodes, TodoProgram::TODO_EDIT_CANCEL_PORT, 1476);
    let todo_begin_edit = append_source_port(&mut nodes, TodoProgram::TODO_BEGIN_EDIT_PORT, 1473);
    append_field_read(&mut nodes, todo_begin_edit, "id", 1551);
    append_field_read(&mut nodes, todo_begin_edit, "title", 1552);
    let gated_edited_todo_list = append_non_empty_value_or_skip(
        &mut nodes,
        trimmed_edit_title,
        NodeId(1434),
        NodeId(1462),
        1461,
        1463,
        NodeId(1433),
    );
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1562),
        vec![NodeId(1444)],
        &[
            TriggeredUpdateConfig {
                source: NodeId(1446),
                body: NodeId(1447),
                then_node_id: 1448,
            },
            TriggeredUpdateConfig {
                source: NodeId(1452),
                body: NodeId(1451),
                then_node_id: 1453,
            },
            TriggeredUpdateConfig {
                source: NodeId(1455),
                body: NodeId(1454),
                then_node_id: 1456,
            },
            TriggeredUpdateConfig {
                source: NodeId(1464),
                body: gated_edited_todo_list,
                then_node_id: 1465,
            },
            TriggeredUpdateConfig {
                source: NodeId(1467),
                body: NodeId(1466),
                then_node_id: 1468,
            },
        ],
        1469,
        TodoProgram::TODOS_LIST_HOLD_NODE.0,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::TODOS_LIST_SINK,
        1470,
    );
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1472),
        Vec::new(),
        &[
            TriggeredUpdateConfig {
                source: NodeId(1473),
                body: NodeId(1551),
                then_node_id: 1474,
            },
            TriggeredUpdateConfig {
                source: NodeId(1464),
                body: NodeId(1472),
                then_node_id: 1475,
            },
            TriggeredUpdateConfig {
                source: NodeId(1476),
                body: NodeId(1472),
                then_node_id: 1477,
            },
        ],
        1478,
        1479,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::EDIT_TARGET_SINK,
        1480,
    );
    append_literal(&mut nodes, KernelValue::from(""), 1482);
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1482),
        vec![NodeId(1459)],
        &[
            TriggeredUpdateConfig {
                source: NodeId(1473),
                body: NodeId(1552),
                then_node_id: 1483,
            },
            TriggeredUpdateConfig {
                source: NodeId(1464),
                body: NodeId(1482),
                then_node_id: 1484,
            },
            TriggeredUpdateConfig {
                source: NodeId(1476),
                body: NodeId(1482),
                then_node_id: 1485,
            },
        ],
        1486,
        1487,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::EDIT_DRAFT_SINK,
        1488,
    );
    append_literal(&mut nodes, KernelValue::from(false), 1490);
    append_literal(&mut nodes, KernelValue::from(true), 1491);
    append_mirror_cell(&mut nodes, TodoProgram::EDIT_FOCUSED_CELL, 1492);
    append_when(
        &mut nodes,
        NodeId(1492),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(1490),
        }],
        NodeId(1433),
        1494,
    );
    let edit_focus_updates = [NodeId(1547), NodeId(1548), NodeId(1549)];
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1490),
        vec![NodeId(1494), edit_focus_updates[0]],
        &[
            TriggeredUpdateConfig {
                source: NodeId(1473),
                body: NodeId(1491),
                then_node_id: 1493,
            },
            TriggeredUpdateConfig {
                source: NodeId(1464),
                body: NodeId(1490),
                then_node_id: 1495,
            },
            TriggeredUpdateConfig {
                source: NodeId(1476),
                body: NodeId(1490),
                then_node_id: 1496,
            },
        ],
        1497,
        1498,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::EDIT_FOCUS_HINT_SINK,
        1499,
    );
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1490),
        vec![NodeId(1492), edit_focus_updates[1], edit_focus_updates[2]],
        &[
            TriggeredUpdateConfig {
                source: NodeId(1473),
                body: NodeId(1490),
                then_node_id: 1500,
            },
            TriggeredUpdateConfig {
                source: NodeId(1464),
                body: NodeId(1490),
                then_node_id: 1501,
            },
            TriggeredUpdateConfig {
                source: NodeId(1476),
                body: NodeId(1490),
                then_node_id: 1502,
            },
        ],
        1503,
        1504,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::EDIT_FOCUSED_SINK,
        1505,
    );
    let hovered_flag = append_field_read(&mut nodes, NodeId(1556), "hovered", 1510);
    append_when(
        &mut nodes,
        hovered_flag,
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(1445),
        }],
        NodeId(1472),
        1511,
    );
    nodes.extend([
        IrNode {
            id: NodeId(1514),
            source_expr: None,
            kind: IrNodeKind::ListRetainObjectBoolField {
                list: NodeId(1430),
                field: "completed".to_string(),
                keep_if: false,
            },
        },
        IrNode {
            id: NodeId(1517),
            source_expr: None,
            kind: IrNodeKind::ListRetainObjectBoolField {
                list: NodeId(1430),
                field: "completed".to_string(),
                keep_if: true,
            },
        },
    ]);
    append_literal(&mut nodes, KernelValue::from(true), 1522);
    append_mirror_cell(&mut nodes, TodoProgram::MAIN_INPUT_FOCUS_HINT_CELL, 1523);
    append_list_count_sink(
        &mut nodes,
        NodeId(1514),
        1515,
        TodoProgram::ACTIVE_COUNT_SINK,
        1516,
    );
    append_list_count_sink(
        &mut nodes,
        NodeId(1517),
        1518,
        TodoProgram::COMPLETED_COUNT_SINK,
        1519,
    );
    append_list_all_object_bool_field_sink(
        &mut nodes,
        NodeId(1430),
        "completed",
        1520,
        TodoProgram::ALL_COMPLETED_SINK,
        1521,
    );
    append_when(
        &mut nodes,
        NodeId(1492),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(1490),
        }],
        NodeId(1433),
        1527,
    );
    append_literal(
        &mut nodes,
        editable_filterable_list_seed_items_value(),
        1562,
    );
    append_source_port(&mut nodes, TodoProgram::MAIN_INPUT_BLUR_PORT, 1540);
    append_source_port(&mut nodes, TodoProgram::MAIN_INPUT_FOCUS_PORT, 1542);
    append_source_port(&mut nodes, TodoProgram::TODO_EDIT_BLUR_PORT, 1545);
    append_source_port(&mut nodes, TodoProgram::TODO_EDIT_FOCUS_PORT, 1546);
    append_triggered_updates(
        &mut nodes,
        &[
            TriggeredUpdateConfig {
                source: NodeId(1546),
                body: NodeId(1490),
                then_node_id: 1547,
            },
            TriggeredUpdateConfig {
                source: NodeId(1545),
                body: NodeId(1490),
                then_node_id: 1548,
            },
            TriggeredUpdateConfig {
                source: NodeId(1546),
                body: NodeId(1491),
                then_node_id: 1549,
            },
        ],
    );
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1426),
        vec![NodeId(1427), NodeId(1527)],
        &[
            TriggeredUpdateConfig {
                source: NodeId(1473),
                body: NodeId(1490),
                then_node_id: 1526,
            },
            TriggeredUpdateConfig {
                source: NodeId(1540),
                body: NodeId(1490),
                then_node_id: 1541,
            },
            TriggeredUpdateConfig {
                source: NodeId(1542),
                body: NodeId(1426),
                then_node_id: 1543,
            },
            TriggeredUpdateConfig {
                source: NodeId(1444),
                body: NodeId(1426),
                then_node_id: 1558,
            },
            TriggeredUpdateConfig {
                source: NodeId(1546),
                body: NodeId(1490),
                then_node_id: 1550,
            },
        ],
        1528,
        1428,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::MAIN_INPUT_FOCUSED_SINK,
        1429,
    );
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1472),
        vec![NodeId(1511)],
        &[
            TriggeredUpdateConfig {
                source: NodeId(1473),
                body: NodeId(1472),
                then_node_id: 1529,
            },
            TriggeredUpdateConfig {
                source: NodeId(1467),
                body: NodeId(1472),
                then_node_id: 1530,
            },
        ],
        1532,
        1512,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::HOVERED_TARGET_SINK,
        1513,
    );
    append_latest_hold_sink_with_triggered_updates(
        &mut nodes,
        NodeId(1522),
        vec![NodeId(1523)],
        &[
            TriggeredUpdateConfig {
                source: NodeId(1401),
                body: NodeId(1490),
                then_node_id: 1533,
            },
            TriggeredUpdateConfig {
                source: NodeId(1404),
                body: NodeId(1490),
                then_node_id: 1534,
            },
            TriggeredUpdateConfig {
                source: NodeId(1407),
                body: NodeId(1490),
                then_node_id: 1535,
            },
            TriggeredUpdateConfig {
                source: NodeId(1452),
                body: NodeId(1490),
                then_node_id: 1536,
            },
            TriggeredUpdateConfig {
                source: NodeId(1455),
                body: NodeId(1490),
                then_node_id: 1537,
            },
            TriggeredUpdateConfig {
                source: NodeId(1473),
                body: NodeId(1490),
                then_node_id: 1538,
            },
            TriggeredUpdateConfig {
                source: NodeId(1542),
                body: NodeId(1490),
                then_node_id: 1544,
            },
            TriggeredUpdateConfig {
                source: NodeId(1444),
                body: NodeId(1491),
                then_node_id: 1559,
            },
        ],
        1539,
        1524,
        LatestHoldMode::AlwaysCreateLatest,
        TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK,
        1525,
    );
    IrProgram {
        nodes,
        functions: Vec::new(),
        persistence: todos_persistence,
    }
}

struct LoweringContext<'a> {
    expressions: &'a [StaticSpannedExpression],
}

type NamedLoweringError = (&'static str, String);
enum LoweringAttemptOutcome {
    Matched(LoweredProgram),
    Rejected(Vec<NamedLoweringError>),
}

trait LoweringFamilyGroup {
    fn try_lower_group(&self, context: &LoweringContext<'_>) -> LoweringAttemptOutcome;
}

trait LoweringSubset {
    fn lowering_subset(&self) -> &'static str;
}

fn lower_labeled_program<T>(
    label: &'static str,
    lower: impl FnOnce() -> Result<T, String>,
    wrap: impl FnOnce(T) -> LoweredProgram,
) -> Result<LoweredProgram, (&'static str, String)> {
    lower().map(wrap).map_err(|error| (label, error))
}

struct BindingsProgramCase<S, T> {
    source: S,
    wrap: fn(T) -> LoweredProgram,
}

struct BindingsProgramGroup<S: 'static, T: 'static> {
    lower_bindings: fn(&[StaticSpannedExpression], S) -> Result<T, String>,
    cases: &'static [BindingsProgramCase<S, T>],
}

fn lower_bindings_program_case<S, T>(
    context: &LoweringContext<'_>,
    group: &BindingsProgramGroup<S, T>,
    case: &BindingsProgramCase<S, T>,
) -> Result<LoweredProgram, (&'static str, String)>
where
    S: LoweringSubset + Clone,
{
    let source = case.source.clone();
    let subset = source.lowering_subset();
    lower_labeled_program(
        subset,
        || (group.lower_bindings)(context.expressions, source),
        case.wrap,
    )
}

struct SurfaceProgramCase<S, T> {
    source: S,
    wrap: fn(T) -> LoweredProgram,
}

struct SurfaceProgramGroup<S: 'static, T: 'static> {
    lower_surface: fn(&[StaticSpannedExpression], S) -> Result<T, String>,
    cases: &'static [SurfaceProgramCase<S, T>],
}

fn lower_surface_program_case<S, T>(
    context: &LoweringContext<'_>,
    group: &SurfaceProgramGroup<S, T>,
    case: &SurfaceProgramCase<S, T>,
) -> Result<LoweredProgram, (&'static str, String)>
where
    S: LoweringSubset + Clone,
{
    let surface = case.source.clone();
    let subset = surface.lowering_subset();
    lower_labeled_program(
        subset,
        || (group.lower_surface)(context.expressions, surface),
        case.wrap,
    )
}

impl<S, T> LoweringFamilyGroup for BindingsProgramGroup<S, T>
where
    S: LoweringSubset + Clone,
{
    fn try_lower_group(&self, context: &LoweringContext<'_>) -> LoweringAttemptOutcome {
        let mut errors = Vec::new();
        for case in self.cases {
            match lower_bindings_program_case(context, self, case) {
                Ok(program) => return LoweringAttemptOutcome::Matched(program),
                Err(error) => errors.push(error),
            }
        }

        LoweringAttemptOutcome::Rejected(errors)
    }
}

impl<S, T> LoweringFamilyGroup for SurfaceProgramGroup<S, T>
where
    S: LoweringSubset + Clone,
{
    fn try_lower_group(&self, context: &LoweringContext<'_>) -> LoweringAttemptOutcome {
        let mut errors = Vec::new();
        for case in self.cases {
            match lower_surface_program_case(context, self, case) {
                Ok(program) => return LoweringAttemptOutcome::Matched(program),
                Err(error) => errors.push(error),
            }
        }

        LoweringAttemptOutcome::Rejected(errors)
    }
}

fn lower_surface_group_typed_program<S, T, U>(
    expressions: &[StaticSpannedExpression],
    _group: &SurfaceProgramGroup<S, T>,
    expected_subset: &'static str,
    extract: impl FnOnce(LoweredProgram) -> Option<U>,
) -> Result<U, String>
where
    S: LoweringSubset + Clone,
{
    let program = lower_program_from_expressions(expressions)?;
    let matched_subset = lowered_program_subset(&program);
    extract(program).ok_or_else(|| {
        format!(
            "generic lowerer matched {}, not {}",
            matched_subset, expected_subset
        )
    })
}

#[derive(Clone, Copy)]
struct GenericHostIrProgramSurfaceConfig<'a> {
    surface: GenericHostIrSurfaceConfig<'a>,
    build_ir: fn() -> IrProgram,
    program: IrHostViewLoweredProgramSpec,
}

impl LoweringSubset for GenericHostIrProgramSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

pub(crate) fn lower_cells_display_typed_program(
    expressions: &[StaticSpannedExpression],
) -> Result<CellsProgram, String> {
    lower_bindings_group_typed_program(
        expressions,
        &DISPLAY_PERSISTENT_SEMANTIC_GROUP,
        "persistent_indexed_text_grid_document",
        |program| match program {
            LoweredProgram::Cells(program) => Some(program),
            _ => None,
        },
    )
}

fn lower_bindings_group_typed_program<S, T, U>(
    expressions: &[StaticSpannedExpression],
    _group: &BindingsProgramGroup<S, T>,
    expected_subset: &'static str,
    extract: impl FnOnce(LoweredProgram) -> Option<U>,
) -> Result<U, String>
where
    S: LoweringSubset + Clone,
{
    let program = lower_program_from_expressions(expressions)?;
    let matched_subset = lowered_program_subset(&program);
    extract(program).ok_or_else(|| {
        format!(
            "generic lowerer matched {}, not {}",
            matched_subset, expected_subset
        )
    })
}

struct BindingsDerivedOutputConfig<'a, S, D, T> {
    shared: &'a S,
    derive: for<'b> fn(
        &'b [StaticSpannedExpression],
        &BTreeMap<String, &'b StaticSpannedExpression>,
        &S,
    ) -> Result<D, String>,
    build_output: for<'b> fn(
        &'b [StaticSpannedExpression],
        BTreeMap<String, &'b StaticSpannedExpression>,
        D,
        &S,
    ) -> Result<T, String>,
}

impl<S, D, T> Copy for BindingsDerivedOutputConfig<'_, S, D, T> {}

impl<S, D, T> Clone for BindingsDerivedOutputConfig<'_, S, D, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S, D, T> LoweringSubset for BindingsDerivedOutputConfig<'_, S, D, T>
where
    S: LoweringSubset,
{
    fn lowering_subset(&self) -> &'static str {
        self.shared.lowering_subset()
    }
}

fn lower_bindings_with_derived_output<S, D, T>(
    expressions: &[StaticSpannedExpression],
    bindings: BTreeMap<String, &StaticSpannedExpression>,
    config: &BindingsDerivedOutputConfig<'_, S, D, T>,
) -> Result<T, String> {
    let derived = (config.derive)(expressions, &bindings, config.shared)?;
    (config.build_output)(expressions, bindings, derived, config.shared)
}

fn lower_bindings_with_derived_output_owned<S, D, T>(
    expressions: &[StaticSpannedExpression],
    config: BindingsDerivedOutputConfig<'static, S, D, T>,
) -> Result<T, String>
where
    S: LoweringSubset,
{
    lower_with_bindings(expressions, |bindings| {
        lower_bindings_with_derived_output(expressions, bindings, &config)
    })
}

type FlatStripeSemanticBindingsConfig<'a, 'b, S> = BindingsDerivedOutputConfig<
    'a,
    FlatStripeSemanticOutputConfig<'a, 'b, S>,
    IrProgram,
    LoweredProgram,
>;

struct FlatStripeSemanticOutputConfig<'a, 'b, S> {
    surface: FlatStripeSurfaceConfig<'a, 'b>,
    semantic: &'a S,
    build_ir:
        for<'c> fn(&BTreeMap<String, &'c StaticSpannedExpression>, &S) -> Result<IrProgram, String>,
    program: IrHostViewLoweredProgramSpec,
}

impl<S> LoweringSubset for FlatStripeSemanticOutputConfig<'_, '_, S> {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

fn derive_flat_stripe_semantic_ir<S>(
    _expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &FlatStripeSemanticOutputConfig<'_, '_, S>,
) -> Result<IrProgram, String> {
    (config.build_ir)(bindings, config.semantic)
}

fn build_flat_stripe_semantic_output<S>(
    expressions: &[StaticSpannedExpression],
    bindings: BTreeMap<String, &StaticSpannedExpression>,
    ir: IrProgram,
    config: &FlatStripeSemanticOutputConfig<'_, '_, S>,
) -> Result<LoweredProgram, String> {
    lower_flat_stripe_surface_program_from_bindings(
        expressions,
        &bindings,
        &config.surface,
        |host_view| build_ir_host_view_lowered_program(ir, host_view, config.program),
    )
}

struct BindingsWithGenericHostIrSemanticOutputConfig<'a, C, S, H> {
    build_context:
        for<'b> fn(&BTreeMap<String, &'b StaticSpannedExpression>, &S, &H) -> Result<C, String>,
    build_host_view: for<'b> fn(
        &'b [StaticSpannedExpression],
        &BTreeMap<String, &'b StaticSpannedExpression>,
        &C,
        &H,
    ) -> Result<HostViewIr, String>,
    build_output: for<'b> fn(
        BTreeMap<String, &'b StaticSpannedExpression>,
        C,
        HostViewIr,
        &S,
    ) -> Result<LoweredProgram, String>,
    semantic: &'a S,
    host_view: &'a H,
}

impl<C, S, H> LoweringSubset for BindingsWithGenericHostIrSemanticOutputConfig<'_, C, S, H>
where
    S: LoweringSubset,
{
    fn lowering_subset(&self) -> &'static str {
        self.semantic.lowering_subset()
    }
}

fn derive_bindings_with_generic_host_ir_semantic_output<C, S, H>(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &BindingsWithGenericHostIrSemanticOutputConfig<'_, C, S, H>,
) -> Result<(C, HostViewIr), String> {
    let derived = (config.build_context)(&bindings, config.semantic, config.host_view)?;
    let host_view = (config.build_host_view)(expressions, &bindings, &derived, config.host_view)?;
    Ok((derived, host_view))
}

fn build_bindings_with_generic_host_ir_semantic_output<C, S, H>(
    _expressions: &[StaticSpannedExpression],
    bindings: BTreeMap<String, &StaticSpannedExpression>,
    derived: (C, HostViewIr),
    config: &BindingsWithGenericHostIrSemanticOutputConfig<'_, C, S, H>,
) -> Result<LoweredProgram, String> {
    let (derived, host_view) = derived;
    (config.build_output)(bindings, derived, host_view, config.semantic)
}

type GenericHostIrSemanticBindingsConfig<'a, C, S, H> = BindingsDerivedOutputConfig<
    'a,
    BindingsWithGenericHostIrSemanticOutputConfig<'a, C, S, H>,
    (C, HostViewIr),
    LoweredProgram,
>;

struct BindingsSingleSinkValueOutputConfig {
    subset: &'static str,
    binding_name: &'static str,
    sink: SinkPortId,
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    derive_sink_value: for<'a> fn(
        &'a [StaticSpannedExpression],
        &BTreeMap<String, &'a StaticSpannedExpression>,
    ) -> Result<KernelValue, String>,
}

impl LoweringSubset for BindingsSingleSinkValueOutputConfig {
    fn lowering_subset(&self) -> &'static str {
        self.subset
    }
}

fn derive_bindings_single_sink_value_output(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &BindingsSingleSinkValueOutputConfig,
) -> Result<KernelValue, String> {
    (config.derive_sink_value)(expressions, bindings)
}

fn build_bindings_single_sink_value_output(
    expressions: &[StaticSpannedExpression],
    bindings: BTreeMap<String, &StaticSpannedExpression>,
    sink_value: KernelValue,
    config: &BindingsSingleSinkValueOutputConfig,
) -> Result<(HostViewIr, BTreeMap<SinkPortId, KernelValue>), String> {
    lower_bindings_single_sink_value_program(
        expressions,
        &bindings,
        config.binding_name,
        config.sink,
        config.view_site,
        config.function_instance,
        sink_value,
        |host_view, sink_values| (host_view, sink_values),
    )
}

type SingleSinkValueBindingsConfig = BindingsDerivedOutputConfig<
    'static,
    BindingsSingleSinkValueOutputConfig,
    KernelValue,
    (HostViewIr, BTreeMap<SinkPortId, KernelValue>),
>;

fn wrap_lowered_program(program: LoweredProgram) -> LoweredProgram {
    program
}

#[derive(Clone, Copy)]
enum PressDrivenAccumulatorLoweredProgramSpec {
    Counter,
}

#[derive(Clone, Copy)]
enum HostViewLoweredProgramSpec {
    NavSelection {
        nav_press_ports: [SourcePortId; 3],
        current_page_sink: SinkPortId,
        title_sink: SinkPortId,
        description_sink: SinkPortId,
        nav_active_sinks: [SinkPortId; 3],
    },
    LatestSignal {
        send_press_ports: [SourcePortId; 2],
        value_sink: SinkPortId,
        sum_sink: SinkPortId,
    },
    ToggleTemplatedLabel {
        toggle_press_port: SourcePortId,
        button_label_sink: SinkPortId,
        label_sink: SinkPortId,
        while_sink: SinkPortId,
    },
    MultiButtonActivation {
        intro_sink: SinkPortId,
        button_press_ports: [SourcePortId; 3],
        button_active_sinks: [SinkPortId; 3],
        state_sink: SinkPortId,
    },
    MultiButtonHover {
        intro_sink: SinkPortId,
        button_press_ports: [SourcePortId; 3],
        button_hover_sinks: [SinkPortId; 3],
    },
    ToggleBranch {
        toggle_press_port: SourcePortId,
        toggle_label_sink: SinkPortId,
        content_sink: SinkPortId,
    },
    SwitchedHoldItems {
        show_item_a_sink: SinkPortId,
        item_count_sinks: [SinkPortId; 2],
        current_item_sink: SinkPortId,
        current_count_sink: SinkPortId,
        item_disabled_sinks: [SinkPortId; 2],
        footer_sink: SinkPortId,
        toggle_press_port: SourcePortId,
        item_press_ports: [SourcePortId; 2],
    },
    FilterableCheckboxList {
        filter_all_port: SourcePortId,
        filter_active_port: SourcePortId,
        filter_sink: SinkPortId,
        checkbox_ports: [SourcePortId; 2],
        checkbox_sinks: [SinkPortId; 2],
        item_label_sinks: [SinkPortId; 2],
        footer_sink: SinkPortId,
    },
    IndependentCheckboxList {
        checkbox_ports: [SourcePortId; 2],
        checkbox_sinks: [SinkPortId; 2],
        label_sinks: [SinkPortId; 2],
        status_sinks: [SinkPortId; 2],
    },
    RemovableCheckboxList {
        add_press_port: SourcePortId,
        clear_completed_port: SourcePortId,
        checkbox_ports: [SourcePortId; 4],
        remove_ports: [SourcePortId; 4],
        checkbox_sinks: [SinkPortId; 4],
        row_label_sinks: [SinkPortId; 4],
        counts_sink: SinkPortId,
        title_sink: SinkPortId,
    },
    DualMappedLabelStripes {
        mode_sink: SinkPortId,
        direct_item_sinks: [SinkPortId; 5],
        block_item_sinks: [SinkPortId; 5],
    },
    IndependentObjectCounters {
        press_ports: [SourcePortId; 3],
        count_sinks: [SinkPortId; 3],
    },
    SelectableRecordColumn {
        title_sink: SinkPortId,
        filter_input_sink: SinkPortId,
        name_input_sink: SinkPortId,
        surname_input_sink: SinkPortId,
        filter_change_port: SourcePortId,
        filter_key_down_port: SourcePortId,
        name_change_port: SourcePortId,
        name_key_down_port: SourcePortId,
        surname_change_port: SourcePortId,
        surname_key_down_port: SourcePortId,
        create_press_port: SourcePortId,
        update_press_port: SourcePortId,
        delete_press_port: SourcePortId,
        row_press_ports: [SourcePortId; 4],
        row_label_sinks: [SinkPortId; 4],
        row_selected_sinks: [SinkPortId; 4],
    },
}

fn build_host_view_lowered_program(
    host_view: HostViewIr,
    spec: HostViewLoweredProgramSpec,
) -> LoweredProgram {
    match spec {
        HostViewLoweredProgramSpec::NavSelection {
            nav_press_ports,
            current_page_sink,
            title_sink,
            description_sink,
            nav_active_sinks,
        } => LoweredProgram::Pages(PagesProgram {
            host_view,
            nav_press_ports,
            current_page_sink,
            title_sink,
            description_sink,
            nav_active_sinks,
        }),
        HostViewLoweredProgramSpec::LatestSignal {
            send_press_ports,
            value_sink,
            sum_sink,
        } => LoweredProgram::Latest(LatestProgram {
            host_view,
            send_press_ports,
            value_sink,
            sum_sink,
        }),
        HostViewLoweredProgramSpec::ToggleTemplatedLabel {
            toggle_press_port,
            button_label_sink,
            label_sink,
            while_sink,
        } => LoweredProgram::TextInterpolationUpdate(TextInterpolationUpdateProgram {
            host_view,
            toggle_press_port,
            button_label_sink,
            label_sink,
            while_sink,
        }),
        HostViewLoweredProgramSpec::MultiButtonActivation {
            intro_sink,
            button_press_ports,
            button_active_sinks,
            state_sink,
        } => LoweredProgram::ButtonHoverToClickTest(ButtonHoverToClickTestProgram {
            host_view,
            intro_sink,
            button_press_ports,
            button_active_sinks,
            state_sink,
        }),
        HostViewLoweredProgramSpec::MultiButtonHover {
            intro_sink,
            button_press_ports,
            button_hover_sinks,
        } => LoweredProgram::ButtonHoverTest(ButtonHoverTestProgram {
            host_view,
            intro_sink,
            button_press_ports,
            button_hover_sinks,
        }),
        HostViewLoweredProgramSpec::ToggleBranch {
            toggle_press_port,
            toggle_label_sink,
            content_sink,
        } => LoweredProgram::WhileFunctionCall(WhileFunctionCallProgram {
            host_view,
            toggle_press_port,
            toggle_label_sink,
            content_sink,
        }),
        HostViewLoweredProgramSpec::SwitchedHoldItems {
            show_item_a_sink,
            item_count_sinks,
            current_item_sink,
            current_count_sink,
            item_disabled_sinks,
            footer_sink,
            toggle_press_port,
            item_press_ports,
        } => LoweredProgram::SwitchHoldTest(SwitchHoldTestProgram {
            host_view,
            show_item_a_sink,
            item_count_sinks,
            current_item_sink,
            current_count_sink,
            item_disabled_sinks,
            footer_sink,
            toggle_press_port,
            item_press_ports,
        }),
        HostViewLoweredProgramSpec::FilterableCheckboxList {
            filter_all_port,
            filter_active_port,
            filter_sink,
            checkbox_ports,
            checkbox_sinks,
            item_label_sinks,
            footer_sink,
        } => LoweredProgram::FilterCheckboxBug(FilterCheckboxBugProgram {
            host_view,
            filter_all_port,
            filter_active_port,
            filter_sink,
            checkbox_ports,
            checkbox_sinks,
            item_label_sinks,
            footer_sink,
        }),
        HostViewLoweredProgramSpec::IndependentCheckboxList {
            checkbox_ports,
            checkbox_sinks,
            label_sinks,
            status_sinks,
        } => LoweredProgram::CheckboxTest(CheckboxTestProgram {
            host_view,
            checkbox_ports,
            checkbox_sinks,
            label_sinks,
            status_sinks,
        }),
        HostViewLoweredProgramSpec::RemovableCheckboxList {
            add_press_port,
            clear_completed_port,
            checkbox_ports,
            remove_ports,
            checkbox_sinks,
            row_label_sinks,
            counts_sink,
            title_sink,
        } => LoweredProgram::ChainedListRemoveBug(ChainedListRemoveBugProgram {
            host_view,
            add_press_port,
            clear_completed_port,
            checkbox_ports,
            remove_ports,
            checkbox_sinks,
            row_label_sinks,
            counts_sink,
            title_sink,
        }),
        HostViewLoweredProgramSpec::DualMappedLabelStripes {
            mode_sink,
            direct_item_sinks,
            block_item_sinks,
        } => LoweredProgram::ListMapBlock(ListMapBlockProgram {
            host_view,
            mode_sink,
            direct_item_sinks,
            block_item_sinks,
        }),
        HostViewLoweredProgramSpec::IndependentObjectCounters {
            press_ports,
            count_sinks,
        } => LoweredProgram::ListObjectState(ListObjectStateProgram {
            host_view,
            press_ports,
            count_sinks,
        }),
        HostViewLoweredProgramSpec::SelectableRecordColumn {
            title_sink,
            filter_input_sink,
            name_input_sink,
            surname_input_sink,
            filter_change_port,
            filter_key_down_port,
            name_change_port,
            name_key_down_port,
            surname_change_port,
            surname_key_down_port,
            create_press_port,
            update_press_port,
            delete_press_port,
            row_press_ports,
            row_label_sinks,
            row_selected_sinks,
        } => LoweredProgram::Crud(CrudProgram {
            host_view,
            title_sink,
            filter_input_sink,
            name_input_sink,
            surname_input_sink,
            filter_change_port,
            filter_key_down_port,
            name_change_port,
            name_key_down_port,
            surname_change_port,
            surname_key_down_port,
            create_press_port,
            update_press_port,
            delete_press_port,
            row_press_ports,
            row_label_sinks,
            row_selected_sinks,
        }),
    }
}

#[derive(Clone, Copy)]
enum HostViewSinkValuesLoweredProgramSpec {
    StaticStackDisplay,
    SequenceMessageDisplay,
    StaticDocumentDisplay,
}

fn build_host_view_sink_values_lowered_program(
    host_view: HostViewIr,
    sink_values: BTreeMap<SinkPortId, KernelValue>,
    spec: HostViewSinkValuesLoweredProgramSpec,
) -> LoweredProgram {
    match spec {
        HostViewSinkValuesLoweredProgramSpec::StaticStackDisplay => {
            LoweredProgram::Layers(LayersProgram {
                host_view,
                sink_values,
            })
        }
        HostViewSinkValuesLoweredProgramSpec::SequenceMessageDisplay => {
            LoweredProgram::Fibonacci(FibonacciProgram {
                host_view,
                sink_values,
            })
        }
        HostViewSinkValuesLoweredProgramSpec::StaticDocumentDisplay => {
            LoweredProgram::StaticDocument(StaticProgram {
                host_view,
                sink_values,
            })
        }
    }
}

#[derive(Clone, Copy)]
enum IrHostViewLoweredProgramSpec {
    EditableFilterableList {
        selected_filter_sink: SinkPortId,
    },
    ExternalModeMappedItems {
        toggle_port: SourcePortId,
        mode_sink: SinkPortId,
        info_sink: SinkPortId,
        items_list_sink: SinkPortId,
        item_sinks: [SinkPortId; 4],
    },
    RetainedToggleFilterList {
        toggle_port: SourcePortId,
        mode_sink: SinkPortId,
        count_sink: SinkPortId,
        items_list_sink: SinkPortId,
        item_sinks: [SinkPortId; 6],
    },
    CountedFilteredAppendList {
        input_sink: SinkPortId,
        all_count_sink: SinkPortId,
        retain_count_sink: SinkPortId,
        items_list_sink: SinkPortId,
        input_change_port: SourcePortId,
        input_key_down_port: SourcePortId,
        item_sinks: [SinkPortId; 4],
    },
    RemovableAppendList {
        title_sink: SinkPortId,
        input_sink: SinkPortId,
        count_sink: SinkPortId,
        items_list_sink: SinkPortId,
        input_change_port: SourcePortId,
        input_key_down_port: SourcePortId,
        item_sinks: [SinkPortId; 6],
    },
    ClearableAppendList {
        title_sink: SinkPortId,
        input_sink: SinkPortId,
        count_sink: SinkPortId,
        items_list_sink: SinkPortId,
        input_change_port: SourcePortId,
        input_key_down_port: SourcePortId,
        clear_press_port: SourcePortId,
        item_sinks: [SinkPortId; 4],
    },
    CanvasHistoryDocument {
        title_sink: SinkPortId,
        count_sink: SinkPortId,
        circles_sink: SinkPortId,
        canvas_click_port: SourcePortId,
        undo_press_port: SourcePortId,
    },
    DualActionAccumulatorDocument {
        decrement_port: SourcePortId,
        increment_port: SourcePortId,
        decrement_hovered_cell: MirrorCellId,
        increment_hovered_cell: MirrorCellId,
        counter_sink: SinkPortId,
        decrement_hovered_sink: SinkPortId,
        increment_hovered_sink: SinkPortId,
        initial_value: i64,
    },
}

fn build_ir_host_view_lowered_program(
    ir: IrProgram,
    host_view: HostViewIr,
    spec: IrHostViewLoweredProgramSpec,
) -> LoweredProgram {
    match spec {
        IrHostViewLoweredProgramSpec::EditableFilterableList {
            selected_filter_sink,
        } => LoweredProgram::TodoMvc(TodoProgram {
            ir,
            host_view,
            selected_filter_sink,
        }),
        IrHostViewLoweredProgramSpec::ExternalModeMappedItems {
            toggle_port,
            mode_sink,
            info_sink,
            items_list_sink,
            item_sinks,
        } => LoweredProgram::ListMapExternalDep(ListMapExternalDepProgram {
            ir,
            host_view,
            toggle_port,
            mode_sink,
            info_sink,
            items_list_sink,
            item_sinks,
        }),
        IrHostViewLoweredProgramSpec::RetainedToggleFilterList {
            toggle_port,
            mode_sink,
            count_sink,
            items_list_sink,
            item_sinks,
        } => LoweredProgram::ListRetainReactive(ListRetainReactiveProgram {
            ir,
            host_view,
            toggle_port,
            mode_sink,
            count_sink,
            items_list_sink,
            item_sinks,
        }),
        IrHostViewLoweredProgramSpec::CountedFilteredAppendList {
            input_sink,
            all_count_sink,
            retain_count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            item_sinks,
        } => LoweredProgram::ListRetainCount(ListRetainCountProgram {
            ir,
            host_view,
            input_sink,
            all_count_sink,
            retain_count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            item_sinks,
        }),
        IrHostViewLoweredProgramSpec::RemovableAppendList {
            title_sink,
            input_sink,
            count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            item_sinks,
        } => LoweredProgram::ListRetainRemove(ListRetainRemoveProgram {
            ir,
            host_view,
            title_sink,
            input_sink,
            count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            item_sinks,
        }),
        IrHostViewLoweredProgramSpec::ClearableAppendList {
            title_sink,
            input_sink,
            count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            clear_press_port,
            item_sinks,
        } => LoweredProgram::ShoppingList(ShoppingListProgram {
            ir,
            host_view,
            title_sink,
            input_sink,
            count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            clear_press_port,
            item_sinks,
        }),
        IrHostViewLoweredProgramSpec::CanvasHistoryDocument {
            title_sink,
            count_sink,
            circles_sink,
            canvas_click_port,
            undo_press_port,
        } => LoweredProgram::CircleDrawer(CircleDrawerProgram {
            ir,
            host_view,
            title_sink,
            count_sink,
            circles_sink,
            canvas_click_port,
            undo_press_port,
        }),
        IrHostViewLoweredProgramSpec::DualActionAccumulatorDocument {
            decrement_port,
            increment_port,
            decrement_hovered_cell,
            increment_hovered_cell,
            counter_sink,
            decrement_hovered_sink,
            increment_hovered_sink,
            initial_value,
        } => LoweredProgram::ComplexCounter(ComplexCounterProgram {
            ir,
            host_view,
            decrement_port,
            increment_port,
            decrement_hovered_cell,
            increment_hovered_cell,
            counter_sink,
            decrement_hovered_sink,
            increment_hovered_sink,
            initial_value,
        }),
    }
}

macro_rules! define_source_parsed_entrypoint {
    ($public_name:ident, $private_name:ident, $program_ty:ty) => {
        pub fn $public_name(source: &str) -> Result<$program_ty, String> {
            let expressions = parse_static_expressions(source)?;
            $private_name(&expressions)
        }
    };
}

#[derive(Clone, Copy)]
struct GenericHostProgramSurfaceConfig<'a> {
    surface: GenericHostSurfaceConfig<'a>,
    program: HostViewLoweredProgramSpec,
}

impl LoweringSubset for GenericHostProgramSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

fn lower_generic_host_program_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: GenericHostProgramSurfaceConfig<'static>,
) -> Result<LoweredProgram, String> {
    let host_view = lower_generic_host_surface_owned(expressions, config.surface)?;
    Ok(build_host_view_lowered_program(host_view, config.program))
}

const GENERIC_HOST_SURFACE_GROUP: SurfaceProgramGroup<
    GenericHostProgramSurfaceConfig<'static>,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_generic_host_program_surface_owned,
    cases: &[
        SurfaceProgramCase {
            source: GenericHostProgramSurfaceConfig {
                surface: GenericHostSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "nav_selection_document",
                        top_level_bindings: &["store", "current_route", "current_page", "document"],
                        required_paths: &[],
                        hold_paths: &[],
                        required_functions: &["nav_button", "page"],
                        alias_paths: &[
                            ["nav", "home", "event", "press"].as_slice(),
                            ["nav", "about", "event", "press"].as_slice(),
                            ["nav", "contact", "event", "press"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Router", "route"].as_slice(),
                            ["Router", "go_to"].as_slice(),
                            ["Element", "button"].as_slice(),
                        ],
                        text_fragments: &["Home", "About", "Contact", "404 - Not Found"],
                        require_hold: false,
                        require_latest: false,
                        require_then: true,
                        require_when: true,
                        require_while: true,
                    },
                    sink_bindings: &[
                        ("current_page", SinkPortId(1994)),
                        ("store.nav.home.active", SinkPortId(1991)),
                        ("store.nav.about.active", SinkPortId(1992)),
                        ("store.nav.contact.active", SinkPortId(1993)),
                    ],
                    source_bindings: &[
                        ("store.nav.home", SourcePortId(1989)),
                        ("store.nav.about", SourcePortId(1990)),
                        ("store.nav.contact", SourcePortId(1991)),
                    ],
                    view_site: ViewSiteId(1989),
                    function_instance: FunctionInstanceId(1989),
                },
                program: HostViewLoweredProgramSpec::NavSelection {
                    nav_press_ports: [SourcePortId(1989), SourcePortId(1990), SourcePortId(1991)],
                    current_page_sink: SinkPortId(1994),
                    title_sink: SinkPortId(1989),
                    description_sink: SinkPortId(1990),
                    nav_active_sinks: [SinkPortId(1991), SinkPortId(1992), SinkPortId(1993)],
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: GenericHostProgramSurfaceConfig {
                surface: GenericHostSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "latest_signal_document",
                        top_level_bindings: &[
                            "value",
                            "sum",
                            "send_1_button",
                            "send_2_button",
                            "document",
                        ],
                        required_paths: &[],
                        hold_paths: &[],
                        required_functions: &["send_button", "value_container"],
                        alias_paths: &[],
                        function_call_paths: &[
                            ["Math", "sum"].as_slice(),
                            ["Element", "button"].as_slice(),
                        ],
                        text_fragments: &["Send 1", "Send 2", "Sum:"],
                        require_hold: false,
                        require_latest: true,
                        require_then: true,
                        require_when: false,
                        require_while: false,
                    },
                    sink_bindings: &[("value", SinkPortId(1994)), ("sum", SinkPortId(1995))],
                    source_bindings: &[
                        ("send_1_button", SourcePortId(1994)),
                        ("send_2_button", SourcePortId(1995)),
                    ],
                    view_site: ViewSiteId(1998),
                    function_instance: FunctionInstanceId(1998),
                },
                program: HostViewLoweredProgramSpec::LatestSignal {
                    send_press_ports: [SourcePortId(1994), SourcePortId(1995)],
                    value_sink: SinkPortId(1994),
                    sum_sink: SinkPortId(1995),
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: GenericHostProgramSurfaceConfig {
                surface: GenericHostSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "toggle_templated_label_document",
                        top_level_bindings: &["document"],
                        required_paths: &[["store", "toggle"].as_slice()],
                        hold_paths: &[["store", "value"].as_slice()],
                        required_functions: &[],
                        alias_paths: &[["toggle", "event", "press"].as_slice()],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Bool", "not"].as_slice(),
                        ],
                        text_fragments: &[
                            "Toggle (value:",
                            "Label shows:",
                            "WHILE says: True",
                            "WHILE says: False",
                        ],
                        require_hold: true,
                        require_latest: false,
                        require_then: true,
                        require_when: false,
                        require_while: true,
                    },
                    sink_bindings: &[("store.value", SinkPortId(1998))],
                    source_bindings: &[("store.toggle", SourcePortId(1996))],
                    view_site: ViewSiteId(1996),
                    function_instance: FunctionInstanceId(1996),
                },
                program: HostViewLoweredProgramSpec::ToggleTemplatedLabel {
                    toggle_press_port: SourcePortId(1996),
                    button_label_sink: SinkPortId(1996),
                    label_sink: SinkPortId(1997),
                    while_sink: SinkPortId(1998),
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: GenericHostProgramSurfaceConfig {
                surface: GenericHostSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "multi_button_activation_document",
                        top_level_bindings: &[],
                        required_paths: &[
                            ["store", "btn_a"].as_slice(),
                            ["store", "btn_b"].as_slice(),
                            ["store", "btn_c"].as_slice(),
                        ],
                        hold_paths: &[],
                        required_functions: &["make_button"],
                        alias_paths: &[["elements", "button", "event", "press"].as_slice()],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Bool", "not"].as_slice(),
                        ],
                        text_fragments: &[
                            "Click each button - clicked ones turn darker with outline",
                            "Button ",
                            "States - A:",
                        ],
                        require_hold: true,
                        require_latest: false,
                        require_then: true,
                        require_when: false,
                        require_while: false,
                    },
                    sink_bindings: &[
                        ("store.btn_a.clicked", SinkPortId(2024)),
                        ("store.btn_b.clicked", SinkPortId(2025)),
                        ("store.btn_c.clicked", SinkPortId(2026)),
                    ],
                    source_bindings: &[
                        ("store.btn_a.elements.button", SourcePortId(2022)),
                        ("store.btn_b.elements.button", SourcePortId(2023)),
                        ("store.btn_c.elements.button", SourcePortId(2024)),
                    ],
                    view_site: ViewSiteId(2050),
                    function_instance: FunctionInstanceId(2013),
                },
                program: HostViewLoweredProgramSpec::MultiButtonActivation {
                    intro_sink: SinkPortId(2023),
                    button_press_ports: [
                        SourcePortId(2022),
                        SourcePortId(2023),
                        SourcePortId(2024),
                    ],
                    button_active_sinks: [SinkPortId(2024), SinkPortId(2025), SinkPortId(2026)],
                    state_sink: SinkPortId(2027),
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: GenericHostProgramSurfaceConfig {
                surface: GenericHostSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "multi_button_hover_document",
                        top_level_bindings: &[],
                        required_paths: &[],
                        hold_paths: &[],
                        required_functions: &["simple_button"],
                        alias_paths: &[["element", "hovered"].as_slice()],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "button"].as_slice(),
                        ],
                        text_fragments: &[
                            "Hover each button - only hovered one should show border",
                            "Button ",
                        ],
                        require_hold: false,
                        require_latest: false,
                        require_then: false,
                        require_when: false,
                        require_while: true,
                    },
                    sink_bindings: &[
                        ("__simple_button_2067.hovered", SinkPortId(2034)),
                        ("__simple_button_2068.hovered", SinkPortId(2035)),
                        ("__simple_button_2069.hovered", SinkPortId(2036)),
                    ],
                    source_bindings: &[],
                    view_site: ViewSiteId(2063),
                    function_instance: FunctionInstanceId(2015),
                },
                program: HostViewLoweredProgramSpec::MultiButtonHover {
                    intro_sink: SinkPortId(2033),
                    button_press_ports: [
                        SourcePortId(2033),
                        SourcePortId(2034),
                        SourcePortId(2035),
                    ],
                    button_hover_sinks: [SinkPortId(2034), SinkPortId(2035), SinkPortId(2036)],
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: GenericHostProgramSurfaceConfig {
                surface: GenericHostSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "toggle_branch_document",
                        top_level_bindings: &[],
                        required_paths: &[],
                        hold_paths: &[["store", "show_greeting"].as_slice()],
                        required_functions: &["greeting"],
                        alias_paths: &[["toggle", "event", "press"].as_slice()],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Bool", "not"].as_slice(),
                        ],
                        text_fragments: &["Toggle (show:", "Hello, ", "Hidden"],
                        require_hold: false,
                        require_latest: false,
                        require_then: true,
                        require_when: false,
                        require_while: true,
                    },
                    sink_bindings: &[("store.show_greeting", SinkPortId(2022))],
                    source_bindings: &[("store.toggle", SourcePortId(2021))],
                    view_site: ViewSiteId(2021),
                    function_instance: FunctionInstanceId(2021),
                },
                program: HostViewLoweredProgramSpec::ToggleBranch {
                    toggle_press_port: SourcePortId(2021),
                    toggle_label_sink: SinkPortId(2021),
                    content_sink: SinkPortId(2022),
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: GenericHostProgramSurfaceConfig {
                surface: GenericHostSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "switched_hold_items_document",
                        top_level_bindings: &[],
                        required_paths: &[
                            ["store", "item_a"].as_slice(),
                            ["store", "item_b"].as_slice(),
                        ],
                        hold_paths: &[["store", "show_item_a"].as_slice()],
                        required_functions: &["create_item"],
                        alias_paths: &[
                            ["view_toggle", "event", "press"].as_slice(),
                            ["item_elements", "button", "event", "press"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Bool", "not"].as_slice(),
                        ],
                        text_fragments: &[
                            "Showing: Item A",
                            "Showing: Item B",
                            "Toggle View",
                            "Click Item A",
                            "Click Item B",
                            "Counts should increment correctly.",
                        ],
                        require_hold: true,
                        require_latest: false,
                        require_then: true,
                        require_when: false,
                        require_while: true,
                    },
                    sink_bindings: &[
                        ("store.show_item_a", SinkPortId(2033)),
                        ("store.item_a.click_count", SinkPortId(2034)),
                        ("store.item_b.click_count", SinkPortId(2035)),
                    ],
                    source_bindings: &[
                        ("store.view_toggle", SourcePortId(2025)),
                        ("store.item_a.item_elements.button", SourcePortId(2026)),
                        ("store.item_b.item_elements.button", SourcePortId(2027)),
                    ],
                    view_site: ViewSiteId(2055),
                    function_instance: FunctionInstanceId(2014),
                },
                program: HostViewLoweredProgramSpec::SwitchedHoldItems {
                    show_item_a_sink: SinkPortId(2033),
                    item_count_sinks: [SinkPortId(2034), SinkPortId(2035)],
                    current_item_sink: SinkPortId(2028),
                    current_count_sink: SinkPortId(2029),
                    item_disabled_sinks: [SinkPortId(2030), SinkPortId(2031)],
                    footer_sink: SinkPortId(2032),
                    toggle_press_port: SourcePortId(2025),
                    item_press_ports: [SourcePortId(2026), SourcePortId(2027)],
                },
            },
            wrap: wrap_lowered_program,
        },
    ],
};

#[derive(Clone, Copy)]
struct CheckboxListProgramSurfaceConfig<'a, 'b> {
    surface: CheckboxListSurfaceConfig<'a, 'b>,
    program: HostViewLoweredProgramSpec,
}

impl LoweringSubset for CheckboxListProgramSurfaceConfig<'_, '_> {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

fn lower_checkbox_list_program_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: CheckboxListProgramSurfaceConfig<'static, 'static>,
) -> Result<LoweredProgram, String> {
    let host_view = lower_checkbox_list_surface_owned(expressions, config.surface)?;
    Ok(build_host_view_lowered_program(host_view, config.program))
}

const CHECKBOX_LIST_SURFACE_GROUP: SurfaceProgramGroup<
    CheckboxListProgramSurfaceConfig<'static, 'static>,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_checkbox_list_program_surface_owned,
    cases: &[
        SurfaceProgramCase {
            source: CheckboxListProgramSurfaceConfig {
                surface: CheckboxListSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "filterable_checkbox_list_document",
                        top_level_bindings: &[],
                        required_paths: &[],
                        hold_paths: &[["store", "selected_filter"].as_slice()],
                        required_functions: &["create_item", "render_item"],
                        alias_paths: &[
                            ["filter_buttons", "all", "event", "press"].as_slice(),
                            ["filter_buttons", "active", "event", "press"].as_slice(),
                            ["toggle_all_checkbox", "event", "click"].as_slice(),
                            ["elements", "checkbox", "event", "click"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Element", "checkbox"].as_slice(),
                            ["List", "retain"].as_slice(),
                            ["List", "map"].as_slice(),
                            ["Bool", "not"].as_slice(),
                        ],
                        text_fragments: &[
                            "Filter: ",
                            "All",
                            "Active",
                            "checked: ",
                            "Test: Click Active, All, then checkbox 3x",
                        ],
                        require_hold: false,
                        require_latest: true,
                        require_then: false,
                        require_when: false,
                        require_while: true,
                    },
                    function_instance: FunctionInstanceId(1200),
                    root_view_site: ViewSiteId(1200),
                    container_view_site: ViewSiteId(1201),
                    container_kind: CheckboxListDocumentContainerKind::StyledColumn {
                        gap_px: 12,
                        padding_px: Some(20),
                        width: Some(HostWidth::Px(300)),
                        align_cross: None,
                    },
                    prefix_children: &[
                        CheckboxListDocumentChildConfig::Label {
                            view_site: ViewSiteId(1202),
                            sink: SinkPortId(1200),
                        },
                        CheckboxListDocumentChildConfig::PlainButtonRow(PlainButtonRowConfig {
                            function_instance: FunctionInstanceId(1200),
                            row_view_site: ViewSiteId(1203),
                            button_view_site: ViewSiteId(1204),
                            buttons: &[
                                PlainButtonRowButtonConfig {
                                    mapped_item_identity: 1,
                                    label: "All",
                                    press_port: SourcePortId(1200),
                                },
                                PlainButtonRowButtonConfig {
                                    mapped_item_identity: 2,
                                    label: "Active",
                                    press_port: SourcePortId(1201),
                                },
                            ],
                        }),
                    ],
                    rows_container_view_site: Some(ViewSiteId(1206)),
                    rows: &MappedCheckboxRowsConfig {
                        function_instance: FunctionInstanceId(1200),
                        row_kind: MappedCheckboxRowKind::PlainStripe,
                        row_view_site: ViewSiteId(1207),
                        checkbox_view_site: ViewSiteId(1208),
                        label_view_site: ViewSiteId(1209),
                        checkbox_sinks: &[SinkPortId(1201), SinkPortId(1202)],
                        checkbox_ports: &[SourcePortId(1202), SourcePortId(1203)],
                        label_sinks: &[SinkPortId(1203), SinkPortId(1204)],
                        status_view_site: None,
                        status_sinks: &[],
                        action_button_view_site: None,
                        action_button_label: None,
                        action_button_ports: &[],
                    },
                    suffix_children: &[CheckboxListDocumentChildConfig::Label {
                        view_site: ViewSiteId(1210),
                        sink: SinkPortId(1205),
                    }],
                },
                program: HostViewLoweredProgramSpec::FilterableCheckboxList {
                    filter_all_port: SourcePortId(1200),
                    filter_active_port: SourcePortId(1201),
                    filter_sink: SinkPortId(1200),
                    checkbox_ports: [SourcePortId(1202), SourcePortId(1203)],
                    checkbox_sinks: [SinkPortId(1201), SinkPortId(1202)],
                    item_label_sinks: [SinkPortId(1203), SinkPortId(1204)],
                    footer_sink: SinkPortId(1205),
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: CheckboxListProgramSurfaceConfig {
                surface: CheckboxListSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "independent_checkbox_list_document",
                        top_level_bindings: &[],
                        required_paths: &[],
                        hold_paths: &[],
                        required_functions: &["make_item"],
                        alias_paths: &[["checkbox_link", "event", "click"].as_slice()],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "checkbox"].as_slice(),
                            ["List", "map"].as_slice(),
                            ["Bool", "not"].as_slice(),
                        ],
                        text_fragments: &["Item A", "Item B", "(checked)", "(unchecked)"],
                        require_hold: true,
                        require_latest: false,
                        require_then: false,
                        require_when: false,
                        require_while: false,
                    },
                    function_instance: FunctionInstanceId(1300),
                    root_view_site: ViewSiteId(1300),
                    container_view_site: ViewSiteId(1301),
                    container_kind: CheckboxListDocumentContainerKind::Stripe,
                    prefix_children: &[],
                    rows_container_view_site: None,
                    rows: &MappedCheckboxRowsConfig {
                        function_instance: FunctionInstanceId(1300),
                        row_kind: MappedCheckboxRowKind::PlainStripe,
                        row_view_site: ViewSiteId(1302),
                        checkbox_view_site: ViewSiteId(1303),
                        label_view_site: ViewSiteId(1304),
                        checkbox_sinks: &[SinkPortId(1300), SinkPortId(1301)],
                        checkbox_ports: &[SourcePortId(1300), SourcePortId(1301)],
                        label_sinks: &[SinkPortId(1304), SinkPortId(1305)],
                        status_view_site: Some(ViewSiteId(1305)),
                        status_sinks: &[SinkPortId(1302), SinkPortId(1303)],
                        action_button_view_site: None,
                        action_button_label: None,
                        action_button_ports: &[],
                    },
                    suffix_children: &[],
                },
                program: HostViewLoweredProgramSpec::IndependentCheckboxList {
                    checkbox_ports: [SourcePortId(1300), SourcePortId(1301)],
                    checkbox_sinks: [SinkPortId(1300), SinkPortId(1301)],
                    label_sinks: [SinkPortId(1304), SinkPortId(1305)],
                    status_sinks: [SinkPortId(1302), SinkPortId(1303)],
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: CheckboxListProgramSurfaceConfig {
                surface: CheckboxListSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "removable_checkbox_list_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[
                            ["store", "elements", "add_button"].as_slice(),
                            ["store", "elements", "clear_completed_button"].as_slice(),
                            ["store", "item_to_add"].as_slice(),
                            ["store", "items"].as_slice(),
                            ["store", "active_items"].as_slice(),
                            ["store", "completed_items"].as_slice(),
                        ],
                        hold_paths: &[["store", "next_id"].as_slice()],
                        required_functions: &["create_item", "render_item"],
                        alias_paths: &[
                            ["elements", "add_button", "event", "press"].as_slice(),
                            ["elements", "clear_completed_button", "event", "press"].as_slice(),
                            ["item", "elements", "remove_button", "event", "press"].as_slice(),
                            ["elements", "checkbox", "event", "click"].as_slice(),
                            ["item", "completed"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Element", "checkbox"].as_slice(),
                            ["List", "append"].as_slice(),
                            ["List", "remove"].as_slice(),
                            ["List", "retain"].as_slice(),
                            ["List", "map"].as_slice(),
                            ["Bool", "not"].as_slice(),
                        ],
                        text_fragments: &[
                            "Add Item",
                            "Clear completed",
                            "Active: ",
                            "Completed: ",
                            "Item A",
                            "Item B",
                            "Pass: Only Item B remains",
                            "Pass: Only Item A and Item B remain",
                        ],
                        require_hold: true,
                        require_latest: false,
                        require_then: true,
                        require_when: true,
                        require_while: true,
                    },
                    function_instance: FunctionInstanceId(1400),
                    root_view_site: ViewSiteId(1400),
                    container_view_site: ViewSiteId(1401),
                    container_kind: CheckboxListDocumentContainerKind::Stripe,
                    prefix_children: &[
                        CheckboxListDocumentChildConfig::Label {
                            view_site: ViewSiteId(1402),
                            sink: SinkPortId(1409),
                        },
                        CheckboxListDocumentChildConfig::PlainButtonRow(PlainButtonRowConfig {
                            function_instance: FunctionInstanceId(1400),
                            row_view_site: ViewSiteId(1403),
                            button_view_site: ViewSiteId(1404),
                            buttons: &[
                                PlainButtonRowButtonConfig {
                                    mapped_item_identity: 1,
                                    label: "Add Item",
                                    press_port: SourcePortId(1400),
                                },
                                PlainButtonRowButtonConfig {
                                    mapped_item_identity: 2,
                                    label: "Clear completed",
                                    press_port: SourcePortId(1401),
                                },
                            ],
                        }),
                    ],
                    rows_container_view_site: Some(ViewSiteId(1406)),
                    rows: &MappedCheckboxRowsConfig {
                        function_instance: FunctionInstanceId(1400),
                        row_kind: MappedCheckboxRowKind::StyledRow { gap_px: 8 },
                        row_view_site: ViewSiteId(1407),
                        checkbox_view_site: ViewSiteId(1408),
                        label_view_site: ViewSiteId(1409),
                        checkbox_sinks: &[
                            SinkPortId(1400),
                            SinkPortId(1401),
                            SinkPortId(1402),
                            SinkPortId(1403),
                        ],
                        checkbox_ports: &[
                            SourcePortId(1402),
                            SourcePortId(1403),
                            SourcePortId(1404),
                            SourcePortId(1405),
                        ],
                        label_sinks: &[
                            SinkPortId(1404),
                            SinkPortId(1405),
                            SinkPortId(1406),
                            SinkPortId(1407),
                        ],
                        status_view_site: None,
                        status_sinks: &[],
                        action_button_view_site: Some(ViewSiteId(1411)),
                        action_button_label: Some("X"),
                        action_button_ports: &[
                            SourcePortId(1410),
                            SourcePortId(1411),
                            SourcePortId(1412),
                            SourcePortId(1413),
                        ],
                    },
                    suffix_children: &[CheckboxListDocumentChildConfig::Label {
                        view_site: ViewSiteId(1410),
                        sink: SinkPortId(1408),
                    }],
                },
                program: HostViewLoweredProgramSpec::RemovableCheckboxList {
                    add_press_port: SourcePortId(1400),
                    clear_completed_port: SourcePortId(1401),
                    checkbox_ports: [
                        SourcePortId(1402),
                        SourcePortId(1403),
                        SourcePortId(1404),
                        SourcePortId(1405),
                    ],
                    remove_ports: [
                        SourcePortId(1410),
                        SourcePortId(1411),
                        SourcePortId(1412),
                        SourcePortId(1413),
                    ],
                    checkbox_sinks: [
                        SinkPortId(1400),
                        SinkPortId(1401),
                        SinkPortId(1402),
                        SinkPortId(1403),
                    ],
                    row_label_sinks: [
                        SinkPortId(1404),
                        SinkPortId(1405),
                        SinkPortId(1406),
                        SinkPortId(1407),
                    ],
                    counts_sink: SinkPortId(1408),
                    title_sink: SinkPortId(1409),
                },
            },
            wrap: wrap_lowered_program,
        },
    ],
};

#[derive(Clone)]
struct FlatStripeHostOnlyProgramSurfaceConfig<'a, 'b> {
    surface: FlatStripeHostOnlySurfaceConfig<'a, 'b>,
    program: HostViewLoweredProgramSpec,
}

impl LoweringSubset for FlatStripeHostOnlyProgramSurfaceConfig<'_, '_> {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

fn lower_flat_stripe_host_only_program_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: FlatStripeHostOnlyProgramSurfaceConfig<'static, 'static>,
) -> Result<LoweredProgram, String> {
    let host_view = lower_flat_stripe_host_only_surface_owned(expressions, config.surface)?;
    Ok(build_host_view_lowered_program(host_view, config.program))
}

const LIST_HOST_ONLY_SURFACE_GROUP: SurfaceProgramGroup<
    FlatStripeHostOnlyProgramSurfaceConfig<'static, 'static>,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_flat_stripe_host_only_program_surface_owned,
    cases: &[
        SurfaceProgramCase {
            source: FlatStripeHostOnlyProgramSurfaceConfig {
                surface: FlatStripeHostOnlySurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "dual_mapped_label_stripes_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[
                            ["store", "mode"].as_slice(),
                            ["store", "items"].as_slice(),
                        ],
                        hold_paths: &[],
                        required_functions: &[],
                        alias_paths: &[
                            ["store", "mode"].as_slice(),
                            ["store", "items"].as_slice(),
                            ["item"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["List", "map"].as_slice(),
                            ["Bool", "or"].as_slice(),
                        ],
                        text_fragments: &["Mode: ", "All"],
                        require_hold: false,
                        require_latest: false,
                        require_then: false,
                        require_when: true,
                        require_while: true,
                    },
                    document: FlatStripeDocumentConfig {
                        function_instance: FunctionInstanceId(50),
                        root_view_site: ViewSiteId(50),
                        stripe_view_site: ViewSiteId(51),
                        children: &[
                            FlatStripeDocumentChildConfig::Label {
                                view_site: ViewSiteId(52),
                                sink: SinkPortId(50),
                            },
                            FlatStripeDocumentChildConfig::MappedLabelList(MappedLabelListConfig {
                                function_instance: Some(FunctionInstanceId(50)),
                                list_view_site: ViewSiteId(53),
                                list_item_view_site: ViewSiteId(54),
                                sink_start: 51,
                                count: 5,
                                label_kind: MappedLabelNodeKind::Plain,
                            }),
                            FlatStripeDocumentChildConfig::MappedLabelList(MappedLabelListConfig {
                                function_instance: Some(FunctionInstanceId(50)),
                                list_view_site: ViewSiteId(55),
                                list_item_view_site: ViewSiteId(56),
                                sink_start: 56,
                                count: 5,
                                label_kind: MappedLabelNodeKind::Plain,
                            }),
                        ],
                    },
                },
                program: HostViewLoweredProgramSpec::DualMappedLabelStripes {
                    mode_sink: SinkPortId(50),
                    direct_item_sinks: [
                        SinkPortId(51),
                        SinkPortId(52),
                        SinkPortId(53),
                        SinkPortId(54),
                        SinkPortId(55),
                    ],
                    block_item_sinks: [
                        SinkPortId(56),
                        SinkPortId(57),
                        SinkPortId(58),
                        SinkPortId(59),
                        SinkPortId(60),
                    ],
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: FlatStripeHostOnlyProgramSurfaceConfig {
                surface: FlatStripeHostOnlySurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "independent_object_counters_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[["store", "counters"].as_slice()],
                        hold_paths: &[],
                        required_functions: &["make_counter"],
                        alias_paths: &[
                            ["button", "event", "press"].as_slice(),
                            ["state"].as_slice(),
                            ["store", "counters"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["List", "map"].as_slice(),
                        ],
                        text_fragments: &[
                            "Click each button - counts should be independent",
                            "Count: ",
                            "Click me",
                        ],
                        require_hold: true,
                        require_latest: false,
                        require_then: true,
                        require_when: false,
                        require_while: false,
                    },
                    document: FlatStripeDocumentConfig {
                        function_instance: FunctionInstanceId(90),
                        root_view_site: ViewSiteId(90),
                        stripe_view_site: ViewSiteId(91),
                        children: &[
                            FlatStripeDocumentChildConfig::Label {
                                view_site: ViewSiteId(92),
                                sink: SinkPortId(89),
                            },
                            FlatStripeDocumentChildConfig::MappedButtonLabelRows {
                                container_view_site: ViewSiteId(93),
                                rows: MappedButtonLabelRowsConfig {
                                    function_instance: FunctionInstanceId(90),
                                    row_view_site: ViewSiteId(94),
                                    button_view_site: ViewSiteId(95),
                                    button_label: "Click me",
                                    button_press_ports: &[
                                        SourcePortId(90),
                                        SourcePortId(91),
                                        SourcePortId(92),
                                    ],
                                    label_view_site: ViewSiteId(96),
                                    label_sinks: &[SinkPortId(90), SinkPortId(91), SinkPortId(92)],
                                },
                            },
                        ],
                    },
                },
                program: HostViewLoweredProgramSpec::IndependentObjectCounters {
                    press_ports: [SourcePortId(90), SourcePortId(91), SourcePortId(92)],
                    count_sinks: [SinkPortId(90), SinkPortId(91), SinkPortId(92)],
                },
            },
            wrap: wrap_lowered_program,
        },
    ],
};

const LIST_BOOL_TOGGLE_SEMANTIC_BINDINGS_GROUP: BindingsProgramGroup<
    FlatStripeSemanticBindingsConfig<'static, 'static, BoolToggleListSemanticConfig<'static>>,
    LoweredProgram,
> = BindingsProgramGroup {
    lower_bindings: lower_bindings_with_derived_output_owned::<
        FlatStripeSemanticOutputConfig<'static, 'static, BoolToggleListSemanticConfig<'static>>,
        IrProgram,
        LoweredProgram,
    >,
    cases: &[
        BindingsProgramCase {
            source: BindingsDerivedOutputConfig {
                shared: &FlatStripeSemanticOutputConfig {
                    surface: FlatStripeSurfaceConfig {
                        validation: StructuralValidationSpec {
                            subset: "external_mode_mapped_items_document",
                            top_level_bindings: &["store", "document"],
                            required_paths: &[
                                ["store", "filter_button"].as_slice(),
                                ["store", "items"].as_slice(),
                            ],
                            hold_paths: &[["store", "show_filtered"].as_slice()],
                            required_functions: &[],
                            alias_paths: &[
                                ["filter_button", "event", "press"].as_slice(),
                                ["store", "show_filtered"].as_slice(),
                                ["store", "items"].as_slice(),
                                ["item", "show_when_filtered"].as_slice(),
                                ["item", "name"].as_slice(),
                            ],
                            function_call_paths: &[
                                ["Document", "new"].as_slice(),
                                ["Element", "stripe"].as_slice(),
                                ["Element", "label"].as_slice(),
                                ["Element", "button"].as_slice(),
                                ["List", "map"].as_slice(),
                                ["Bool", "not"].as_slice(),
                            ],
                            text_fragments: &[
                                "Toggle filter",
                                "Expected: When True, show Apple and Cherry. When False, show all.",
                                "Apple",
                                "Banana",
                                "Cherry",
                                "Date",
                            ],
                            require_hold: true,
                            require_latest: false,
                            require_then: true,
                            require_when: false,
                            require_while: true,
                        },
                        document: FlatStripeDocumentConfig {
                            function_instance: FunctionInstanceId(40),
                            root_view_site: ViewSiteId(40),
                            stripe_view_site: ViewSiteId(41),
                            children: &[
                                FlatStripeDocumentChildConfig::Label {
                                    view_site: ViewSiteId(42),
                                    sink: SinkPortId(40),
                                },
                                FlatStripeDocumentChildConfig::Button {
                                    view_site: ViewSiteId(43),
                                    label: "Toggle filter",
                                    press_port: SourcePortId(40),
                                },
                                FlatStripeDocumentChildConfig::Label {
                                    view_site: ViewSiteId(44),
                                    sink: SinkPortId(41),
                                },
                                FlatStripeDocumentChildConfig::MappedLabelList(
                                    MappedLabelListConfig {
                                        function_instance: Some(FunctionInstanceId(40)),
                                        list_view_site: ViewSiteId(45),
                                        list_item_view_site: ViewSiteId(46),
                                        sink_start: 42,
                                        count: 4,
                                        label_kind: MappedLabelNodeKind::Plain,
                                    },
                                ),
                            ],
                        },
                    },
                    semantic: const {
                        &BoolToggleListSemanticConfig {
                            subset: "external_mode_mapped_items_document",
                            program: BoolToggleListProgramConfig {
                                runtime: BoolToggleRuntimeConfig {
                                    base_node_id: 4000,
                                    toggle_press_port: SourcePortId(40),
                                    mode_sink: SinkPortId(40),
                                    initial_value: false,
                                    true_label: "show_filtered: True",
                                    false_label: "show_filtered: False",
                                },
                                items_list_sink: SinkPortId(46),
                                aux: BoolToggleListAuxConfig::StaticText {
                                    sink: SinkPortId(41),
                                    text: "Expected: When True, show Apple and Cherry. When False, show all.",
                                },
                            },
                            persistence: &[],
                            derivation: BoolToggleListValueDerivation::StaticValues(
                                StaticBoolToggleListValuesConfig {
                                    initial_value: false,
                                    false_values: &["Apple", "Banana", "Cherry", "Date"],
                                    true_values: &["Apple", "Cherry"],
                                },
                            ),
                        }
                    },
                    build_ir: lower_bool_toggle_list_semantic_ir,
                    program: IrHostViewLoweredProgramSpec::ExternalModeMappedItems {
                        toggle_port: SourcePortId(40),
                        mode_sink: SinkPortId(40),
                        info_sink: SinkPortId(41),
                        items_list_sink: SinkPortId(46),
                        item_sinks: [
                            SinkPortId(42),
                            SinkPortId(43),
                            SinkPortId(44),
                            SinkPortId(45),
                        ],
                    },
                },
                derive: derive_flat_stripe_semantic_ir,
                build_output: build_flat_stripe_semantic_output,
            },
            wrap: wrap_lowered_program,
        },
        BindingsProgramCase {
            source: BindingsDerivedOutputConfig {
                shared: &FlatStripeSemanticOutputConfig {
                    surface: FlatStripeSurfaceConfig {
                        validation: StructuralValidationSpec {
                            subset: "retained_toggle_filter_list_document",
                            top_level_bindings: &["store", "document"],
                            required_paths: &[
                                ["store", "show_even"].as_slice(),
                                ["store", "numbers"].as_slice(),
                                ["store", "filtered"].as_slice(),
                            ],
                            hold_paths: &[["store", "show_even"].as_slice()],
                            required_functions: &[],
                            alias_paths: &[["store", "toggle", "event", "press"].as_slice()],
                            function_call_paths: &[
                                ["Bool", "not"].as_slice(),
                                ["Bool", "or"].as_slice(),
                                ["List", "retain"].as_slice(),
                                ["List", "count"].as_slice(),
                                ["List", "map"].as_slice(),
                                ["Element", "button"].as_slice(),
                                ["Element", "label"].as_slice(),
                            ],
                            text_fragments: &["Toggle filter", "Filtered count:"],
                            require_hold: true,
                            require_latest: false,
                            require_then: true,
                            require_when: true,
                            require_while: false,
                        },
                        document: FlatStripeDocumentConfig {
                            function_instance: FunctionInstanceId(30),
                            root_view_site: ViewSiteId(30),
                            stripe_view_site: ViewSiteId(31),
                            children: &[
                                FlatStripeDocumentChildConfig::Button {
                                    view_site: ViewSiteId(32),
                                    label: "Toggle filter",
                                    press_port: SourcePortId(30),
                                },
                                FlatStripeDocumentChildConfig::Label {
                                    view_site: ViewSiteId(33),
                                    sink: SinkPortId(30),
                                },
                                FlatStripeDocumentChildConfig::Label {
                                    view_site: ViewSiteId(34),
                                    sink: SinkPortId(31),
                                },
                                FlatStripeDocumentChildConfig::MappedLabelList(
                                    MappedLabelListConfig {
                                        function_instance: Some(FunctionInstanceId(30)),
                                        list_view_site: ViewSiteId(35),
                                        list_item_view_site: ViewSiteId(36),
                                        sink_start: 32,
                                        count: 6,
                                        label_kind: MappedLabelNodeKind::Plain,
                                    },
                                ),
                            ],
                        },
                    },
                    semantic: const {
                        &BoolToggleListSemanticConfig {
                            subset: "retained_toggle_filter_list_document",
                            program: BoolToggleListProgramConfig {
                                runtime: BoolToggleRuntimeConfig {
                                    base_node_id: 3000,
                                    toggle_press_port: SourcePortId(30),
                                    mode_sink: SinkPortId(30),
                                    initial_value: false,
                                    true_label: "show_even: True",
                                    false_label: "show_even: False",
                                },
                                items_list_sink: SinkPortId(38),
                                aux: BoolToggleListAuxConfig::CountText {
                                    sink: SinkPortId(31),
                                    prefix: "Filtered count: ",
                                },
                            },
                            persistence: &[LoweringPathPersistenceConfig {
                                path: &["store", "show_even"],
                                node: NodeId(3004),
                                local_slot: 0,
                                persist_kind: PersistKind::Hold,
                            }],
                            derivation: BoolToggleListValueDerivation::HoldBackedIntegerSubset(
                                HoldBackedIntegerSubsetBoolToggleListValuesConfig {
                                    toggle_path: &["store", "show_even"],
                                    source_list_path: &["store", "numbers"],
                                    selected_subset_path: &["store", "filtered"],
                                    item_alias: "n",
                                },
                            ),
                        }
                    },
                    build_ir: lower_bool_toggle_list_semantic_ir,
                    program: IrHostViewLoweredProgramSpec::RetainedToggleFilterList {
                        toggle_port: SourcePortId(30),
                        mode_sink: SinkPortId(30),
                        count_sink: SinkPortId(31),
                        items_list_sink: SinkPortId(38),
                        item_sinks: [
                            SinkPortId(32),
                            SinkPortId(33),
                            SinkPortId(34),
                            SinkPortId(35),
                            SinkPortId(36),
                            SinkPortId(37),
                        ],
                    },
                },
                derive: derive_flat_stripe_semantic_ir,
                build_output: build_flat_stripe_semantic_output,
            },
            wrap: wrap_lowered_program,
        },
    ],
};

const LIST_PERSISTENT_SEMANTIC_BINDINGS_GROUP: BindingsProgramGroup<
    PersistentSemanticBindingsConfig<'static, ExecutorDerivedHostViewProgramConfig, LoweredProgram>,
    LoweredProgram,
> = BindingsProgramGroup {
    lower_bindings: lower_bindings_with_derived_output_owned::<
        PersistentSemanticOutputConfig<
            'static,
            ExecutorDerivedHostViewProgramConfig,
            LoweredProgram,
        >,
        IrProgram,
        LoweredProgram,
    >,
    cases: &[BindingsProgramCase {
        source: BindingsDerivedOutputConfig {
            shared: &PersistentSemanticOutputConfig {
                validation: const {
                    StructuralValidationSpec {
                        subset: "editable_filterable_list_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[],
                        hold_paths: &[],
                        required_functions: &["new_todo"],
                        alias_paths: &[
                            [
                                "todo",
                                "todo_elements",
                                "todo_title_element",
                                "event",
                                "double_click",
                            ]
                            .as_slice(),
                            ["store", "elements", "toggle_all_checkbox", "event", "click"]
                                .as_slice(),
                        ],
                        function_call_paths: &[
                            ["Router", "go_to"].as_slice(),
                            ["Router", "route"].as_slice(),
                            ["Element", "text_input"].as_slice(),
                            ["Element", "checkbox"].as_slice(),
                            ["Element", "button"].as_slice(),
                        ],
                        text_fragments: &[
                            "Double-click to edit a todo",
                            "Created by",
                            "Martin Kavík",
                        ],
                        require_hold: false,
                        require_latest: false,
                        require_then: false,
                        require_when: false,
                        require_while: false,
                    }
                },
                build_ir: lower_editable_filterable_list_ui_state_ir,
                persistence: const {
                    &[LoweringPathPersistenceConfig {
                        path: &["store", "todos"],
                        node: TodoProgram::TODOS_LIST_HOLD_NODE,
                        local_slot: 0,
                        persist_kind: PersistKind::ListStore,
                    }]
                },
                semantic: ExecutorDerivedHostViewProgramConfig {
                    derive_sink_values: derive_editable_filterable_list_host_view_sink_values,
                    host_view_template: editable_filterable_list_host_view_template,
                    program: IrHostViewLoweredProgramSpec::EditableFilterableList {
                        selected_filter_sink: TodoProgram::SELECTED_FILTER_SINK,
                    },
                },
                build_output: build_persistent_executor_derived_host_view_output,
            },
            derive: derive_persistent_semantic_ir,
            build_output: build_persistent_semantic_output,
        },
        wrap: wrap_lowered_program,
    }],
};

#[derive(Clone)]
struct AppendListProgramSurfaceConfig<'a> {
    surface: AppendListSurfaceConfig<'a>,
    program: IrHostViewLoweredProgramSpec,
}

impl LoweringSubset for AppendListProgramSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

fn lower_append_list_program_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: AppendListProgramSurfaceConfig<'static>,
) -> Result<LoweredProgram, String> {
    let (mut ir, host_view) = lower_append_list_surface_owned(expressions, config.surface)?;
    // Add persistence for the items hold node in shopping_list (clearable_append_list_document)
    // The items hold node is NodeId(10023) for the shopping_list example
    if let IrHostViewLoweredProgramSpec::ClearableAppendList {
        items_list_sink, ..
    } = &config.program
    {
        // Find the hold node that feeds into items_list_sink
        for node in &ir.nodes {
            if let crate::ir::IrNodeKind::SinkPort { port, input } = &node.kind {
                if *port == *items_list_sink {
                    // Found the sink for the items list, check if input is a hold node
                    if let Some(hold_node) = ir.nodes.iter().find(|n| n.id == *input) {
                        if matches!(hold_node.kind, crate::ir::IrNodeKind::Hold { .. }) {
                            ir.persistence.push(crate::ir::IrNodePersistence {
                                node: hold_node.id,
                                policy: crate::ir::PersistPolicy::Durable {
                                    root_key: boon::parser::PersistenceId::new(),
                                    local_slot: 0,
                                    persist_kind: crate::ir::PersistKind::ListStore,
                                },
                            });
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(build_ir_host_view_lowered_program(
        ir,
        host_view,
        config.program,
    ))
}

#[derive(Clone)]
struct TitledColumnProgramSurfaceConfig<'a> {
    surface: TitledColumnSurfaceConfig<'a>,
    program: HostViewLoweredProgramSpec,
}

impl LoweringSubset for TitledColumnProgramSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

fn lower_titled_column_program_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: TitledColumnProgramSurfaceConfig<'static>,
) -> Result<LoweredProgram, String> {
    let host_view = lower_titled_column_surface_owned(expressions, config.surface)?;
    Ok(build_host_view_lowered_program(host_view, config.program))
}

const LIST_APPEND_LIST_SURFACE_GROUP: SurfaceProgramGroup<
    AppendListProgramSurfaceConfig<'static>,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_append_list_program_surface_owned,
    cases: &[
        SurfaceProgramCase {
            source: AppendListProgramSurfaceConfig {
                surface: AppendListSurfaceConfig {
                    validation: AppendListValidationSpec {
                        subset: "counted_filtered_append_list_document",
                        required_paths: &[
                            ["store", "input"].as_slice(),
                            ["store", "text_to_add"].as_slice(),
                            ["store", "items"].as_slice(),
                        ],
                        required_functions: &[
                            "root_element",
                            "all_count_label",
                            "retain_count_label",
                        ],
                        alias_paths: &[
                            ["store", "input", "event", "key_down", "key"].as_slice(),
                            ["store", "input", "text"].as_slice(),
                            ["element", "event", "change", "text"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "text_input"].as_slice(),
                            ["Text", "empty"].as_slice(),
                            ["List", "append"].as_slice(),
                            ["List", "map"].as_slice(),
                            ["List", "count"].as_slice(),
                            ["List", "retain"].as_slice(),
                        ],
                        text_fragments: &[
                            "Type and press Enter",
                            "All count: ",
                            "Retain count: ",
                            "Initial",
                        ],
                        require_latest: true,
                        require_then: false,
                        require_when: true,
                    },
                    runtime: AppendListRuntimeConfig {
                        title: None,
                        input_change_port: SourcePortId(70),
                        input_key_down_port: SourcePortId(71),
                        input_sink: SinkPortId(70),
                        count_sink: SinkPortId(71),
                        count_prefix: "All count: ",
                        count_suffix: None,
                        derived_count_sinks: &[AppendListDerivedCountConfig {
                            list: AppendListDerivedListSpec::RetainLiteralBool(true),
                            sink: SinkPortId(72),
                            prefix: "Retain count: ",
                            suffix: None,
                        }],
                        items_list_sink: SinkPortId(77),
                        clear_press_port: None,
                        initial_items: &["Initial"],
                        base_node_id: 7000,
                    },
                    host_view: AppendListHostViewConfig::FlatStripe(
                        AppendListFlatStripeDocumentConfig {
                            function_instance: FunctionInstanceId(70),
                            root_view_site: ViewSiteId(70),
                            stripe_view_site: ViewSiteId(71),
                            title: None,
                            input_view_site: ViewSiteId(72),
                            input_sink: SinkPortId(70),
                            placeholder: "Type and press Enter",
                            input_change_port: SourcePortId(70),
                            input_key_down_port: SourcePortId(71),
                            focus_on_mount: true,
                            labels: &[
                                (ViewSiteId(73), SinkPortId(71)),
                                (ViewSiteId(74), SinkPortId(72)),
                            ],
                            mapped_list: MappedLabelListConfig {
                                function_instance: Some(FunctionInstanceId(70)),
                                list_view_site: ViewSiteId(75),
                                list_item_view_site: ViewSiteId(76),
                                sink_start: 73,
                                count: 4,
                                label_kind: MappedLabelNodeKind::Plain,
                            },
                        },
                    ),
                },
                program: IrHostViewLoweredProgramSpec::CountedFilteredAppendList {
                    input_sink: SinkPortId(70),
                    all_count_sink: SinkPortId(71),
                    retain_count_sink: SinkPortId(72),
                    items_list_sink: SinkPortId(77),
                    input_change_port: SourcePortId(70),
                    input_key_down_port: SourcePortId(71),
                    item_sinks: [
                        SinkPortId(73),
                        SinkPortId(74),
                        SinkPortId(75),
                        SinkPortId(76),
                    ],
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: AppendListProgramSurfaceConfig {
                surface: AppendListSurfaceConfig {
                    validation: AppendListValidationSpec {
                        subset: "removable_append_list_document",
                        required_paths: &[
                            ["store", "input"].as_slice(),
                            ["store", "text_to_add"].as_slice(),
                            ["store", "items"].as_slice(),
                        ],
                        required_functions: &[],
                        alias_paths: &[
                            ["input", "event", "key_down", "key"].as_slice(),
                            ["input", "text"].as_slice(),
                            ["element", "event", "change", "text"].as_slice(),
                            ["store", "text_to_add"].as_slice(),
                            ["store", "items"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "text_input"].as_slice(),
                            ["Text", "trim"].as_slice(),
                            ["Text", "is_not_empty"].as_slice(),
                            ["Text", "empty"].as_slice(),
                            ["List", "append"].as_slice(),
                            ["List", "map"].as_slice(),
                        ],
                        text_fragments: &[
                            "Add items with Enter",
                            "Type and press Enter",
                            "Count: ",
                            "- ",
                            "Apple",
                            "Banana",
                            "Cherry",
                        ],
                        require_latest: true,
                        require_then: true,
                        require_when: true,
                    },
                    runtime: AppendListRuntimeConfig {
                        title: Some(("Add items with Enter", SinkPortId(80))),
                        input_change_port: SourcePortId(80),
                        input_key_down_port: SourcePortId(81),
                        input_sink: SinkPortId(81),
                        count_sink: SinkPortId(82),
                        count_prefix: "Count: ",
                        count_suffix: None,
                        derived_count_sinks: &[],
                        items_list_sink: SinkPortId(89),
                        clear_press_port: None,
                        initial_items: &["Apple", "Banana", "Cherry"],
                        base_node_id: 8000,
                    },
                    host_view: AppendListHostViewConfig::FlatStripe(
                        AppendListFlatStripeDocumentConfig {
                            function_instance: FunctionInstanceId(80),
                            root_view_site: ViewSiteId(80),
                            stripe_view_site: ViewSiteId(81),
                            title: Some((ViewSiteId(82), SinkPortId(80))),
                            input_view_site: ViewSiteId(83),
                            input_sink: SinkPortId(81),
                            placeholder: "Type and press Enter",
                            input_change_port: SourcePortId(80),
                            input_key_down_port: SourcePortId(81),
                            focus_on_mount: true,
                            labels: &[(ViewSiteId(84), SinkPortId(82))],
                            mapped_list: MappedLabelListConfig {
                                function_instance: Some(FunctionInstanceId(80)),
                                list_view_site: ViewSiteId(85),
                                list_item_view_site: ViewSiteId(86),
                                sink_start: 83,
                                count: 6,
                                label_kind: MappedLabelNodeKind::Plain,
                            },
                        },
                    ),
                },
                program: IrHostViewLoweredProgramSpec::RemovableAppendList {
                    title_sink: SinkPortId(80),
                    input_sink: SinkPortId(81),
                    count_sink: SinkPortId(82),
                    items_list_sink: SinkPortId(89),
                    input_change_port: SourcePortId(80),
                    input_key_down_port: SourcePortId(81),
                    item_sinks: [
                        SinkPortId(83),
                        SinkPortId(84),
                        SinkPortId(85),
                        SinkPortId(86),
                        SinkPortId(87),
                        SinkPortId(88),
                    ],
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: AppendListProgramSurfaceConfig {
                surface: AppendListSurfaceConfig {
                    validation: AppendListValidationSpec {
                        subset: "clearable_append_list_document",
                        required_paths: &[
                            ["store", "elements", "item_input"].as_slice(),
                            ["store", "elements", "clear_button"].as_slice(),
                            ["store", "text_to_add"].as_slice(),
                            ["store", "items"].as_slice(),
                        ],
                        required_functions: &[
                            "root_element",
                            "header",
                            "item_input",
                            "items_list",
                            "footer",
                            "item_count_label",
                            "clear_button",
                        ],
                        alias_paths: &[
                            ["elements", "item_input", "event", "key_down", "key"].as_slice(),
                            ["elements", "item_input", "text"].as_slice(),
                            ["elements", "clear_button", "event", "press"].as_slice(),
                            ["element", "event", "change", "text"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "text_input"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Text", "trim"].as_slice(),
                            ["Text", "is_not_empty"].as_slice(),
                            ["Text", "empty"].as_slice(),
                            ["List", "append"].as_slice(),
                            ["List", "clear"].as_slice(),
                            ["List", "map"].as_slice(),
                            ["List", "count"].as_slice(),
                        ],
                        text_fragments: &[
                            "Shopping List",
                            "Type and press Enter to add...",
                            " items",
                            "Clear",
                        ],
                        require_latest: true,
                        require_then: true,
                        require_when: true,
                    },
                    runtime: AppendListRuntimeConfig {
                        title: Some(("Shopping List", SinkPortId(1006))),
                        input_change_port: SourcePortId(1000),
                        input_key_down_port: SourcePortId(1001),
                        input_sink: SinkPortId(1000),
                        count_sink: SinkPortId(1001),
                        count_prefix: "",
                        count_suffix: Some(" items"),
                        derived_count_sinks: &[],
                        items_list_sink: SinkPortId(1007),
                        clear_press_port: Some(SourcePortId(1002)),
                        initial_items: &[],
                        base_node_id: 10000,
                    },
                    host_view: AppendListHostViewConfig::TitledColumn(TitledColumnDocumentConfig {
                        function_instance: FunctionInstanceId(1000),
                        root_view_site: ViewSiteId(1000),
                        stripe_view_site: ViewSiteId(1001),
                        gap_px: 16,
                        padding_px: Some(20),
                        width: Some(HostWidth::Px(400)),
                        title_view_site: ViewSiteId(1002),
                        title_sink: SinkPortId(1006),
                        title_font_size_px: 24,
                        body_children: &[
                            StaticHostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1003),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: None,
                                },
                                kind: StaticHostViewKind::StyledTextInput {
                                    value_sink: SinkPortId(1000),
                                    placeholder: "Type and press Enter to add...",
                                    change_port: SourcePortId(1000),
                                    key_down_port: SourcePortId(1001),
                                    focus_on_mount: true,
                                    disabled_sink: None,
                                    width: Some(HostWidth::Fill),
                                },
                                children: &[],
                            },
                            StaticHostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1004),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: None,
                                },
                                kind: StaticHostViewKind::StripeLayout {
                                    direction: HostStripeDirection::Column,
                                    gap_px: 4,
                                    padding_px: None,
                                    width: None,
                                    align_cross: None,
                                },
                                children: &[
                                    StaticHostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1005),
                                            function_instance: Some(FunctionInstanceId(1000)),
                                            mapped_item_identity: Some(1),
                                        },
                                        kind: StaticHostViewKind::StyledLabel {
                                            sink: SinkPortId(1002),
                                            font_size_px: None,
                                            bold: false,
                                            color: Some("white"),
                                        },
                                        children: &[],
                                    },
                                    StaticHostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1005),
                                            function_instance: Some(FunctionInstanceId(1000)),
                                            mapped_item_identity: Some(2),
                                        },
                                        kind: StaticHostViewKind::StyledLabel {
                                            sink: SinkPortId(1003),
                                            font_size_px: None,
                                            bold: false,
                                            color: Some("white"),
                                        },
                                        children: &[],
                                    },
                                    StaticHostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1005),
                                            function_instance: Some(FunctionInstanceId(1000)),
                                            mapped_item_identity: Some(3),
                                        },
                                        kind: StaticHostViewKind::StyledLabel {
                                            sink: SinkPortId(1004),
                                            font_size_px: None,
                                            bold: false,
                                            color: Some("white"),
                                        },
                                        children: &[],
                                    },
                                    StaticHostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1005),
                                            function_instance: Some(FunctionInstanceId(1000)),
                                            mapped_item_identity: Some(4),
                                        },
                                        kind: StaticHostViewKind::StyledLabel {
                                            sink: SinkPortId(1005),
                                            font_size_px: None,
                                            bold: false,
                                            color: Some("white"),
                                        },
                                        children: &[],
                                    },
                                ],
                            },
                            StaticHostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1006),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: None,
                                },
                                kind: StaticHostViewKind::StripeLayout {
                                    direction: HostStripeDirection::Row,
                                    gap_px: 16,
                                    padding_px: None,
                                    width: None,
                                    align_cross: None,
                                },
                                children: &[
                                    StaticHostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1007),
                                            function_instance: Some(FunctionInstanceId(1000)),
                                            mapped_item_identity: None,
                                        },
                                        kind: StaticHostViewKind::StyledLabel {
                                            sink: SinkPortId(1001),
                                            font_size_px: None,
                                            bold: false,
                                            color: Some("oklch(0.5 0 0)"),
                                        },
                                        children: &[],
                                    },
                                    StaticHostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1008),
                                            function_instance: Some(FunctionInstanceId(1000)),
                                            mapped_item_identity: None,
                                        },
                                        kind: StaticHostViewKind::StyledButton {
                                            label: "Clear",
                                            press_port: SourcePortId(1002),
                                            disabled_sink: None,
                                            width: None,
                                            padding_px: Some(10),
                                            rounded_fully: false,
                                            background: None,
                                            background_sink: None,
                                            active_background: None,
                                            outline_sink: None,
                                            active_outline: None,
                                        },
                                        children: &[],
                                    },
                                ],
                            },
                        ],
                    }),
                },
                program: IrHostViewLoweredProgramSpec::ClearableAppendList {
                    title_sink: SinkPortId(1006),
                    input_sink: SinkPortId(1000),
                    count_sink: SinkPortId(1001),
                    items_list_sink: SinkPortId(1007),
                    input_change_port: SourcePortId(1000),
                    input_key_down_port: SourcePortId(1001),
                    clear_press_port: SourcePortId(1002),
                    item_sinks: [
                        SinkPortId(1002),
                        SinkPortId(1003),
                        SinkPortId(1004),
                        SinkPortId(1005),
                    ],
                },
            },
            wrap: wrap_lowered_program,
        },
    ],
};

const LIST_TITLED_COLUMN_SURFACE_GROUP: SurfaceProgramGroup<
    TitledColumnProgramSurfaceConfig<'static>,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_titled_column_program_surface_owned,
    cases: &[SurfaceProgramCase {
        source: TitledColumnProgramSurfaceConfig {
            surface: TitledColumnSurfaceConfig {
                validation: const {
                    StructuralValidationSpec {
                        subset: "selectable_record_column_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[
                            ["store", "elements", "filter_input"].as_slice(),
                            ["store", "elements", "name_input"].as_slice(),
                            ["store", "elements", "surname_input"].as_slice(),
                            ["store", "elements", "create_button"].as_slice(),
                            ["store", "elements", "update_button"].as_slice(),
                            ["store", "elements", "delete_button"].as_slice(),
                            ["store", "person_to_add"].as_slice(),
                            ["store", "selected_id"].as_slice(),
                            ["store", "people"].as_slice(),
                        ],
                        hold_paths: &[["store", "selected_id"].as_slice()],
                        required_functions: &[
                            "root_element",
                            "filter_row",
                            "filter_input",
                            "content_row",
                            "people_list",
                            "person_row",
                            "form_section",
                            "name_row",
                            "surname_row",
                            "name_input",
                            "surname_input",
                            "button_row",
                            "new_person",
                        ],
                        alias_paths: &[
                            ["elements", "create_button", "event", "press"].as_slice(),
                            ["elements", "delete_button", "event", "press"].as_slice(),
                            ["store", "elements", "update_button", "event", "press"].as_slice(),
                            ["elements", "name_input", "text"].as_slice(),
                            ["elements", "surname_input", "text"].as_slice(),
                            ["person", "id"].as_slice(),
                            ["element", "event", "change", "text"].as_slice(),
                            ["store", "selected_id"].as_slice(),
                            ["id"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "text_input"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["List", "append"].as_slice(),
                            ["List", "remove"].as_slice(),
                            ["List", "retain"].as_slice(),
                            ["List", "map"].as_slice(),
                            ["List", "latest"].as_slice(),
                            ["Text", "empty"].as_slice(),
                            ["Text", "starts_with"].as_slice(),
                            ["Ulid", "generate"].as_slice(),
                        ],
                        text_fragments: &[
                            "CRUD",
                            "Filter prefix:",
                            "Filter by surname",
                            "Name:",
                            "Surname:",
                            "Create",
                            "Update",
                            "Delete",
                            "Hans",
                            "Max",
                            "Roman",
                        ],
                        require_hold: true,
                        require_latest: true,
                        require_then: true,
                        require_when: true,
                        require_while: true,
                    }
                },
                document: TitledColumnDocumentConfig {
                    function_instance: FunctionInstanceId(1700),
                    root_view_site: ViewSiteId(1700),
                    stripe_view_site: ViewSiteId(1701),
                    gap_px: 16,
                    padding_px: Some(20),
                    width: Some(HostWidth::Px(500)),
                    title_view_site: ViewSiteId(1702),
                    title_sink: SinkPortId(1600),
                    title_font_size_px: 22,
                    body_children: &[
                        StaticHostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(1703),
                                function_instance: Some(FunctionInstanceId(1700)),
                                mapped_item_identity: None,
                            },
                            kind: StaticHostViewKind::StyledTextInput {
                                value_sink: SinkPortId(1601),
                                placeholder: "Filter by surname",
                                change_port: SourcePortId(1600),
                                key_down_port: SourcePortId(1601),
                                focus_on_mount: false,
                                disabled_sink: None,
                                width: Some(HostWidth::Px(200)),
                            },
                            children: &[],
                        },
                        StaticHostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(1704),
                                function_instance: Some(FunctionInstanceId(1700)),
                                mapped_item_identity: None,
                            },
                            kind: StaticHostViewKind::StripeLayout {
                                direction: HostStripeDirection::Row,
                                gap_px: 16,
                                padding_px: None,
                                width: None,
                                align_cross: None,
                            },
                            children: &[
                                StaticHostViewNode {
                                    retained_key: RetainedNodeKey {
                                        view_site: ViewSiteId(1705),
                                        function_instance: Some(FunctionInstanceId(1700)),
                                        mapped_item_identity: None,
                                    },
                                    kind: StaticHostViewKind::StripeLayout {
                                        direction: HostStripeDirection::Column,
                                        gap_px: 2,
                                        padding_px: None,
                                        width: Some(HostWidth::Px(250)),
                                        align_cross: None,
                                    },
                                    children: &[
                                        StaticHostViewNode {
                                            retained_key: RetainedNodeKey {
                                                view_site: ViewSiteId(1713),
                                                function_instance: Some(FunctionInstanceId(1700)),
                                                mapped_item_identity: Some(1),
                                            },
                                            kind: StaticHostViewKind::StyledActionLabel {
                                                sink: SinkPortId(1604),
                                                press_port: SourcePortId(1609),
                                                width: Some(HostWidth::Fill),
                                                bold_sink: Some(SinkPortId(1608)),
                                            },
                                            children: &[],
                                        },
                                        StaticHostViewNode {
                                            retained_key: RetainedNodeKey {
                                                view_site: ViewSiteId(1713),
                                                function_instance: Some(FunctionInstanceId(1700)),
                                                mapped_item_identity: Some(2),
                                            },
                                            kind: StaticHostViewKind::StyledActionLabel {
                                                sink: SinkPortId(1605),
                                                press_port: SourcePortId(1610),
                                                width: Some(HostWidth::Fill),
                                                bold_sink: Some(SinkPortId(1609)),
                                            },
                                            children: &[],
                                        },
                                        StaticHostViewNode {
                                            retained_key: RetainedNodeKey {
                                                view_site: ViewSiteId(1713),
                                                function_instance: Some(FunctionInstanceId(1700)),
                                                mapped_item_identity: Some(3),
                                            },
                                            kind: StaticHostViewKind::StyledActionLabel {
                                                sink: SinkPortId(1606),
                                                press_port: SourcePortId(1611),
                                                width: Some(HostWidth::Fill),
                                                bold_sink: Some(SinkPortId(1610)),
                                            },
                                            children: &[],
                                        },
                                        StaticHostViewNode {
                                            retained_key: RetainedNodeKey {
                                                view_site: ViewSiteId(1713),
                                                function_instance: Some(FunctionInstanceId(1700)),
                                                mapped_item_identity: Some(4),
                                            },
                                            kind: StaticHostViewKind::StyledActionLabel {
                                                sink: SinkPortId(1607),
                                                press_port: SourcePortId(1612),
                                                width: Some(HostWidth::Fill),
                                                bold_sink: Some(SinkPortId(1611)),
                                            },
                                            children: &[],
                                        },
                                    ],
                                },
                                StaticHostViewNode {
                                    retained_key: RetainedNodeKey {
                                        view_site: ViewSiteId(1706),
                                        function_instance: Some(FunctionInstanceId(1700)),
                                        mapped_item_identity: None,
                                    },
                                    kind: StaticHostViewKind::StripeLayout {
                                        direction: HostStripeDirection::Column,
                                        gap_px: 10,
                                        padding_px: None,
                                        width: None,
                                        align_cross: None,
                                    },
                                    children: &[
                                        StaticHostViewNode {
                                            retained_key: RetainedNodeKey {
                                                view_site: ViewSiteId(1707),
                                                function_instance: Some(FunctionInstanceId(1700)),
                                                mapped_item_identity: None,
                                            },
                                            kind: StaticHostViewKind::StyledTextInput {
                                                value_sink: SinkPortId(1602),
                                                placeholder: "Name",
                                                change_port: SourcePortId(1602),
                                                key_down_port: SourcePortId(1603),
                                                focus_on_mount: false,
                                                disabled_sink: None,
                                                width: Some(HostWidth::Px(150)),
                                            },
                                            children: &[],
                                        },
                                        StaticHostViewNode {
                                            retained_key: RetainedNodeKey {
                                                view_site: ViewSiteId(1708),
                                                function_instance: Some(FunctionInstanceId(1700)),
                                                mapped_item_identity: None,
                                            },
                                            kind: StaticHostViewKind::StyledTextInput {
                                                value_sink: SinkPortId(1603),
                                                placeholder: "Surname",
                                                change_port: SourcePortId(1604),
                                                key_down_port: SourcePortId(1605),
                                                focus_on_mount: false,
                                                disabled_sink: None,
                                                width: Some(HostWidth::Px(150)),
                                            },
                                            children: &[],
                                        },
                                    ],
                                },
                            ],
                        },
                        StaticHostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(1709),
                                function_instance: Some(FunctionInstanceId(1700)),
                                mapped_item_identity: None,
                            },
                            kind: StaticHostViewKind::StripeLayout {
                                direction: HostStripeDirection::Row,
                                gap_px: 10,
                                padding_px: None,
                                width: None,
                                align_cross: None,
                            },
                            children: &[
                                StaticHostViewNode {
                                    retained_key: RetainedNodeKey {
                                        view_site: ViewSiteId(1710),
                                        function_instance: Some(FunctionInstanceId(1700)),
                                        mapped_item_identity: Some(1),
                                    },
                                    kind: StaticHostViewKind::StyledButton {
                                        label: "Create",
                                        press_port: SourcePortId(1606),
                                        disabled_sink: None,
                                        width: None,
                                        padding_px: None,
                                        rounded_fully: false,
                                        background: None,
                                        background_sink: None,
                                        active_background: None,
                                        outline_sink: None,
                                        active_outline: None,
                                    },
                                    children: &[],
                                },
                                StaticHostViewNode {
                                    retained_key: RetainedNodeKey {
                                        view_site: ViewSiteId(1710),
                                        function_instance: Some(FunctionInstanceId(1700)),
                                        mapped_item_identity: Some(2),
                                    },
                                    kind: StaticHostViewKind::StyledButton {
                                        label: "Update",
                                        press_port: SourcePortId(1607),
                                        disabled_sink: None,
                                        width: None,
                                        padding_px: None,
                                        rounded_fully: false,
                                        background: None,
                                        background_sink: None,
                                        active_background: None,
                                        outline_sink: None,
                                        active_outline: None,
                                    },
                                    children: &[],
                                },
                                StaticHostViewNode {
                                    retained_key: RetainedNodeKey {
                                        view_site: ViewSiteId(1710),
                                        function_instance: Some(FunctionInstanceId(1700)),
                                        mapped_item_identity: Some(3),
                                    },
                                    kind: StaticHostViewKind::StyledButton {
                                        label: "Delete",
                                        press_port: SourcePortId(1608),
                                        disabled_sink: None,
                                        width: None,
                                        padding_px: None,
                                        rounded_fully: false,
                                        background: None,
                                        background_sink: None,
                                        active_background: None,
                                        outline_sink: None,
                                        active_outline: None,
                                    },
                                    children: &[],
                                },
                            ],
                        },
                    ],
                },
            },
            program: HostViewLoweredProgramSpec::SelectableRecordColumn {
                title_sink: SinkPortId(1600),
                filter_input_sink: SinkPortId(1601),
                name_input_sink: SinkPortId(1602),
                surname_input_sink: SinkPortId(1603),
                filter_change_port: SourcePortId(1600),
                filter_key_down_port: SourcePortId(1601),
                name_change_port: SourcePortId(1602),
                name_key_down_port: SourcePortId(1603),
                surname_change_port: SourcePortId(1604),
                surname_key_down_port: SourcePortId(1605),
                create_press_port: SourcePortId(1606),
                update_press_port: SourcePortId(1607),
                delete_press_port: SourcePortId(1608),
                row_press_ports: [
                    SourcePortId(1609),
                    SourcePortId(1610),
                    SourcePortId(1611),
                    SourcePortId(1612),
                ],
                row_label_sinks: [
                    SinkPortId(1604),
                    SinkPortId(1605),
                    SinkPortId(1606),
                    SinkPortId(1607),
                ],
                row_selected_sinks: [
                    SinkPortId(1608),
                    SinkPortId(1609),
                    SinkPortId(1610),
                    SinkPortId(1611),
                ],
            },
        },
        wrap: wrap_lowered_program,
    }],
};

fn derive_sequence_message_sink_value(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<KernelValue, String> {
    let function = require_top_level_function_expr(expressions, "fibonacci", "fibonacci")?;
    let position = require_top_level_binding_expr(bindings, "fibonacci", "position")?;
    let result = require_top_level_binding_expr(bindings, "fibonacci", "result")?;
    let message = require_top_level_binding_expr(bindings, "fibonacci", "message")?;
    let document = require_top_level_binding_expr(bindings, "fibonacci", "document")?;

    ensure_sequence_function(function)?;
    let position = extract_integer_literal(position)?;
    if position < 0 {
        return Err("fibonacci subset requires non-negative `position`".to_string());
    }
    ensure_sequence_result_binding(result)?;
    ensure_sequence_message(message)?;
    ensure_sequence_document(document)?;

    Ok(KernelValue::from(format!(
        "{position}. Fibonacci number is {}",
        compute_sequence_number(position as u64)
    )))
}

type LayersDisplaySinkValuesSurfaceConfig =
    GenericHostSinkValuesSurfaceConfig<'static, [(SinkPortId, StaticKernelValue); 3]>;

#[derive(Clone)]
struct DisplaySinkValueProgramSurfaceConfig {
    surface: LayersDisplaySinkValuesSurfaceConfig,
    program: HostViewSinkValuesLoweredProgramSpec,
}

impl LoweringSubset for DisplaySinkValueProgramSurfaceConfig {
    fn lowering_subset(&self) -> &'static str {
        self.surface.lowering_subset()
    }
}

fn lower_display_sink_value_program_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: DisplaySinkValueProgramSurfaceConfig,
) -> Result<LoweredProgram, String> {
    let (host_view, sink_values) = lower_generic_host_surface_with_sink_values_owned::<
        [(SinkPortId, StaticKernelValue); 3],
    >(expressions, config.surface)?;
    Ok(build_host_view_sink_values_lowered_program(
        host_view,
        sink_values,
        config.program,
    ))
}

const DISPLAY_SINK_VALUE_SURFACE_GROUP: SurfaceProgramGroup<
    DisplaySinkValueProgramSurfaceConfig,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_display_sink_value_program_surface_owned,
    cases: &[SurfaceProgramCase {
        source: DisplaySinkValueProgramSurfaceConfig {
            surface: GenericHostSinkValuesSurfaceConfig {
                validation: StructuralValidationSpec {
                    subset: "static_stack_display",
                    top_level_bindings: &["document"],
                    required_paths: &[],
                    hold_paths: &[],
                    required_functions: &["card"],
                    alias_paths: &[],
                    function_call_paths: &[
                        &["Document", "new"],
                        &["Element", "stack"],
                        &["Element", "container"],
                    ],
                    text_fragments: &["Red Card", "Green Card", "Blue Card"],
                    require_hold: false,
                    require_latest: false,
                    require_then: false,
                    require_when: false,
                    require_while: false,
                },
                sink_bindings: &[],
                source_bindings: &[],
                view_site: ViewSiteId(1986),
                function_instance: FunctionInstanceId(1986),
                sink_values: [
                    (SinkPortId(1986), StaticKernelValue::Text("Red Card")),
                    (SinkPortId(1987), StaticKernelValue::Text("Green Card")),
                    (SinkPortId(1988), StaticKernelValue::Text("Blue Card")),
                ],
            },
            program: HostViewSinkValuesLoweredProgramSpec::StaticStackDisplay,
        },
        wrap: wrap_lowered_program,
    }],
};

fn build_coordinate_history_ir() -> IrProgram {
    let mut ir = Vec::new();
    append_literal(&mut ir, KernelValue::List(Vec::new()), 1970);
    let draw_event = append_source_port(&mut ir, SourcePortId(1970), 1971);
    let draw_x = append_field_read(&mut ir, draw_event, "x", 1972);
    let draw_y = append_field_read(&mut ir, draw_event, "y", 1973);
    let undo_press = append_source_port(&mut ir, SourcePortId(1971), 1976);
    ir.extend([
        IrNode {
            id: NodeId(1974),
            source_expr: None,
            kind: IrNodeKind::ObjectLiteral {
                fields: vec![("x".to_string(), draw_x), ("y".to_string(), draw_y)],
            },
        },
        IrNode {
            id: NodeId(1978),
            source_expr: None,
            kind: IrNodeKind::ListRemoveLast {
                list: NodeId(1980),
                on: undo_press,
            },
        },
    ]);
    append_literal(&mut ir, KernelValue::from("Circle Drawer"), 1982);
    let draw_circle = append_triggered_updates(
        &mut ir,
        &[TriggeredUpdateConfig {
            source: draw_event,
            body: NodeId(1974),
            then_node_id: 1975,
        }],
    )
    .into_iter()
    .next()
    .expect("circle_drawer should define one draw update");
    append_value_sink(&mut ir, NodeId(1982), SinkPortId(1970), 1985);
    append_prefixed_list_count_sink(
        &mut ir,
        NodeId(1980),
        "Circles: ",
        1981,
        1983,
        1984,
        SinkPortId(1971),
        1986,
    );
    append_value_sink(&mut ir, NodeId(1980), SinkPortId(1972), 1987);
    append_list_state_hold(
        &mut ir,
        NodeId(1980),
        NodeId(1970),
        draw_circle,
        1977,
        vec![NodeId(1978)],
        1979,
        1980,
    );
    ir.into()
}

fn derive_static_document_sink_value(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<KernelValue, String> {
    let root = extract_document_root(
        bindings
            .get("document")
            .ok_or_else(|| "static subset requires `document` binding".to_string())?,
    )
    .map_err(|_| "static subset requires `Document/new(root: ...)`".to_string())?;
    extract_static_kernel_value(root)
}

fn derive_static_document_sink_value_from_expressions(
    _expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<KernelValue, String> {
    derive_static_document_sink_value(bindings)
}

#[derive(Clone, Copy)]
struct SingleSinkValueBindingsProgramConfig {
    source: SingleSinkValueBindingsConfig,
    program: HostViewSinkValuesLoweredProgramSpec,
}

impl LoweringSubset for SingleSinkValueBindingsProgramConfig {
    fn lowering_subset(&self) -> &'static str {
        self.source.lowering_subset()
    }
}

fn lower_single_sink_value_bindings_program_owned(
    expressions: &[StaticSpannedExpression],
    config: SingleSinkValueBindingsProgramConfig,
) -> Result<LoweredProgram, String> {
    let (host_view, sink_values) = lower_bindings_with_derived_output_owned::<
        BindingsSingleSinkValueOutputConfig,
        KernelValue,
        (HostViewIr, BTreeMap<SinkPortId, KernelValue>),
    >(expressions, config.source)?;
    Ok(build_host_view_sink_values_lowered_program(
        host_view,
        sink_values,
        config.program,
    ))
}

const DISPLAY_SINGLE_SINK_BINDINGS_GROUP: BindingsProgramGroup<
    SingleSinkValueBindingsProgramConfig,
    LoweredProgram,
> = BindingsProgramGroup {
    lower_bindings: lower_single_sink_value_bindings_program_owned,
    cases: &[
        BindingsProgramCase {
            source: SingleSinkValueBindingsProgramConfig {
                source: BindingsDerivedOutputConfig {
                    shared: &BindingsSingleSinkValueOutputConfig {
                        subset: "sequence_message_display",
                        binding_name: "message",
                        sink: SinkPortId(1985),
                        view_site: ViewSiteId(1985),
                        function_instance: FunctionInstanceId(1985),
                        derive_sink_value: derive_sequence_message_sink_value,
                    },
                    derive: derive_bindings_single_sink_value_output,
                    build_output: build_bindings_single_sink_value_output,
                },
                program: HostViewSinkValuesLoweredProgramSpec::SequenceMessageDisplay,
            },
            wrap: wrap_lowered_program,
        },
        BindingsProgramCase {
            source: SingleSinkValueBindingsProgramConfig {
                source: BindingsDerivedOutputConfig {
                    shared: &BindingsSingleSinkValueOutputConfig {
                        subset: "static_document_display",
                        binding_name: "document",
                        sink: SinkPortId(200),
                        view_site: ViewSiteId(200),
                        function_instance: FunctionInstanceId(200),
                        derive_sink_value: derive_static_document_sink_value_from_expressions,
                    },
                    derive: derive_bindings_single_sink_value_output,
                    build_output: build_bindings_single_sink_value_output,
                },
                program: HostViewSinkValuesLoweredProgramSpec::StaticDocumentDisplay,
            },
            wrap: wrap_lowered_program,
        },
    ],
};

type PersistentSemanticBindingsConfig<'a, S, T> =
    BindingsDerivedOutputConfig<'a, PersistentSemanticOutputConfig<'a, S, T>, IrProgram, T>;

struct PersistentSemanticOutputConfig<'a, S, T> {
    validation: StructuralValidationSpec<'a>,
    build_ir: fn(Vec<IrNodePersistence>) -> IrProgram,
    persistence: &'a [LoweringPathPersistenceConfig<'a>],
    semantic: S,
    build_output: for<'b> fn(
        &'b [StaticSpannedExpression],
        &BTreeMap<String, &'b StaticSpannedExpression>,
        IrProgram,
        &S,
    ) -> Result<T, String>,
}

impl<S, T> LoweringSubset for PersistentSemanticOutputConfig<'_, S, T> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

struct IndexedTextGridProgramConfig<'a> {
    semantic: IndexedTextGridSemanticFamilyConfig<'a, LoweredCellsFormula, CellsFormulaState>,
    program: IndexedTextGridLoweredProgramSpec,
}

#[derive(Clone, Copy)]
struct IndexedTextGridSemanticFamilyConfig<'a, T, S> {
    document: IndexedTextGridDocumentConfig<'a>,
    grid: IndexedTextGridSpec<'a>,
    map_value: fn(String) -> Option<T>,
    build_state: fn(&BTreeMap<(u32, u32), T>) -> S,
}

#[derive(Clone, Copy)]
struct IndexedTextGridDocumentConfig<'a> {
    expression: IndexedStaticExpressionConfig<'a>,
    document_title_binding_name: &'a str,
    document_title_binding_path: &'a [&'a str],
    document_title_steps: &'a [StaticExpressionTraverseStep<'a>],
    row_count: LiteralU32BindingSpec<'a>,
    col_count: LiteralU32BindingSpec<'a>,
    dynamic_title_bindings: &'a [&'a str],
    static_title: &'static str,
    dynamic_title: &'static str,
    column_headers: IndexedTextGridSpec<'a>,
}

struct IndexedTextGridDocument {
    title: &'static str,
    display_title: String,
    row_count: u32,
    col_count: u32,
    column_headers: Vec<String>,
}

struct IndexedTextGridSemantic<T, S> {
    document: IndexedTextGridDocument,
    grid: BTreeMap<(u32, u32), T>,
    state: S,
}

#[derive(Clone, Copy)]
enum StaticExpressionTraverseStep<'a> {
    FunctionArgument {
        path: &'a [&'a str],
        argument_name: &'a str,
    },
    ListItem(usize),
}

#[derive(Clone, Copy)]
struct IndexedTextGridSpec<'a> {
    function_name: &'a str,
    rows: IndexedTextGridRowRange,
}

#[derive(Clone, Copy)]
struct LiteralU32BindingSpec<'a> {
    binding_name: &'a str,
    default: u32,
}

const DISPLAY_PERSISTENT_SEMANTIC_GROUP: BindingsProgramGroup<
    PersistentSemanticBindingsConfig<
        'static,
        IndexedTextGridProgramConfig<'static>,
        LoweredProgram,
    >,
    LoweredProgram,
> = BindingsProgramGroup {
    lower_bindings: lower_bindings_with_derived_output_owned::<
        PersistentSemanticOutputConfig<
            'static,
            IndexedTextGridProgramConfig<'static>,
            LoweredProgram,
        >,
        IrProgram,
        LoweredProgram,
    >,
    cases: &[BindingsProgramCase {
        source: BindingsDerivedOutputConfig {
            shared: &PersistentSemanticOutputConfig {
                validation: const {
                    StructuralValidationSpec {
                        subset: "persistent_indexed_text_grid_document",
                        top_level_bindings: &[
                            "document",
                            "event_ports",
                            "editing_row",
                            "editing_column",
                            "editing_text",
                            "editing_active",
                            "overrides",
                        ],
                        required_paths: &[],
                        hold_paths: &[],
                        required_functions: &[
                            "matching_overrides",
                            "cell_formula",
                            "compute_value",
                        ],
                        alias_paths: &[],
                        function_call_paths: &[],
                        text_fragments: &[],
                        require_hold: false,
                        require_latest: false,
                        require_then: false,
                        require_when: false,
                        require_while: false,
                    }
                },
                build_ir: build_empty_persistent_ir_program,
                persistence: const {
                    &[LoweringPathPersistenceConfig {
                        path: &["overrides"],
                        node: CellsProgram::OVERRIDES_LIST_HOLD_NODE,
                        local_slot: 0,
                        persist_kind: PersistKind::ListStore,
                    }]
                },
                semantic: IndexedTextGridProgramConfig {
                    semantic: IndexedTextGridSemanticFamilyConfig {
                        document: IndexedTextGridDocumentConfig {
                            expression: IndexedStaticExpressionConfig {
                                subset: "persistent_indexed_text_grid_document",
                                context_label: "grid formula expression",
                                row_aliases: &["row"],
                                column_aliases: &["column", "column_index"],
                                empty_text_function_path: &["Text", "empty"],
                            },
                            document_title_binding_name: "document",
                            document_title_binding_path: &["Document", "new"],
                            document_title_steps: &[
                                StaticExpressionTraverseStep::FunctionArgument {
                                    path: &["Document", "new"],
                                    argument_name: "root",
                                },
                                StaticExpressionTraverseStep::FunctionArgument {
                                    path: &["Element", "stripe"],
                                    argument_name: "items",
                                },
                                StaticExpressionTraverseStep::ListItem(0),
                                StaticExpressionTraverseStep::FunctionArgument {
                                    path: &["Element", "label"],
                                    argument_name: "label",
                                },
                            ],
                            row_count: LiteralU32BindingSpec {
                                binding_name: "row_count",
                                default: 100,
                            },
                            col_count: LiteralU32BindingSpec {
                                binding_name: "col_count",
                                default: 26,
                            },
                            dynamic_title_bindings: &["row_count", "col_count"],
                            static_title: "Cells",
                            dynamic_title: "Cells Dynamic",
                            column_headers: IndexedTextGridSpec {
                                function_name: "column_header",
                                rows: IndexedTextGridRowRange::Fixed(0),
                            },
                        },
                        grid: IndexedTextGridSpec {
                            function_name: "default_formula",
                            rows: IndexedTextGridRowRange::InclusiveFromOneToRowCount,
                        },
                        map_value: map_indexed_grid_formula_text,
                        build_state: build_indexed_grid_formula_state,
                    },
                    program: IndexedTextGridLoweredProgramSpec::Cells,
                },
                build_output: build_persistent_indexed_text_grid_output,
            },
            derive: derive_persistent_semantic_ir,
            build_output: build_persistent_semantic_output,
        },
        wrap: wrap_lowered_program,
    }],
};
fn build_dual_action_mirrored_accumulator_ir() -> IrProgram {
    lower_numeric_accumulator_program(
        &NumericAccumulatorRuntimeConfig {
            base_node_id: 1,
            initial_value: 0,
            actions: &[
                NumericAccumulatorAction {
                    press_port: SourcePortId(10),
                    delta: -1,
                },
                NumericAccumulatorAction {
                    press_port: SourcePortId(11),
                    delta: 1,
                },
            ],
            counter_sink: SinkPortId(10),
            mirror_sinks: &[
                NumericAccumulatorMirrorSink {
                    cell: MirrorCellId(20),
                    sink: SinkPortId(11),
                },
                NumericAccumulatorMirrorSink {
                    cell: MirrorCellId(21),
                    sink: SinkPortId(12),
                },
            ],
        },
        |_| Vec::new(),
    )
}

const GENERIC_HOST_IR_SURFACE_GROUP: SurfaceProgramGroup<
    GenericHostIrProgramSurfaceConfig<'static>,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_generic_host_ir_surface_owned,
    cases: &[
        SurfaceProgramCase {
            source: GenericHostIrProgramSurfaceConfig {
                surface: GenericHostIrSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "canvas_history_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[
                            ["store", "elements", "canvas"].as_slice(),
                            ["store", "elements", "undo_button"].as_slice(),
                            ["store", "circles"].as_slice(),
                            ["store", "count"].as_slice(),
                        ],
                        hold_paths: &[],
                        required_functions: &["root_element"],
                        alias_paths: &[
                            ["elements", "canvas", "event", "click"].as_slice(),
                            ["elements", "undo_button", "event", "press"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Element", "svg"].as_slice(),
                            ["Element", "svg_circle"].as_slice(),
                            ["List", "append"].as_slice(),
                            ["List", "remove_last"].as_slice(),
                            ["List", "count"].as_slice(),
                            ["List", "map"].as_slice(),
                        ],
                        text_fragments: &["Circle Drawer", "Undo", "Circles:"],
                        require_hold: false,
                        require_latest: false,
                        require_then: false,
                        require_when: true,
                        require_while: false,
                    },
                    sink_bindings: &[
                        ("store.count", SinkPortId(1971)),
                        ("store.circles", SinkPortId(1972)),
                    ],
                    source_bindings: &[
                        ("store.elements.canvas", SourcePortId(1970)),
                        ("store.elements.undo_button", SourcePortId(1971)),
                    ],
                    view_site: ViewSiteId(1970),
                    function_instance: FunctionInstanceId(1970),
                    timer_source_children: &[],
                },
                build_ir: build_coordinate_history_ir,
                program: IrHostViewLoweredProgramSpec::CanvasHistoryDocument {
                    title_sink: SinkPortId(1970),
                    count_sink: SinkPortId(1971),
                    circles_sink: SinkPortId(1972),
                    canvas_click_port: SourcePortId(1970),
                    undo_press_port: SourcePortId(1971),
                },
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: GenericHostIrProgramSurfaceConfig {
                surface: GenericHostIrSurfaceConfig {
                    validation: StructuralValidationSpec {
                        subset: "dual_action_accumulator_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[
                            ["store", "elements", "decrement_button"].as_slice(),
                            ["store", "elements", "increment_button"].as_slice(),
                        ],
                        hold_paths: &[["store", "counter"].as_slice()],
                        required_functions: &["root_element", "counter_button"],
                        alias_paths: &[
                            ["elements", "decrement_button", "event", "press"].as_slice(),
                            ["elements", "increment_button", "event", "press"].as_slice(),
                            ["element", "hovered"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "button"].as_slice(),
                        ],
                        text_fragments: &["-", "+"],
                        require_hold: true,
                        require_latest: true,
                        require_then: true,
                        require_when: true,
                        require_while: false,
                    },
                    sink_bindings: &[
                        ("store.counter", SinkPortId(10)),
                        ("store.elements.decrement_button.hovered", SinkPortId(11)),
                        ("store.elements.increment_button.hovered", SinkPortId(12)),
                    ],
                    source_bindings: &[
                        ("store.elements.decrement_button", SourcePortId(10)),
                        ("store.elements.increment_button", SourcePortId(11)),
                    ],
                    view_site: ViewSiteId(20),
                    function_instance: FunctionInstanceId(20),
                    timer_source_children: &[],
                },
                build_ir: build_dual_action_mirrored_accumulator_ir,
                program: IrHostViewLoweredProgramSpec::DualActionAccumulatorDocument {
                    decrement_port: SourcePortId(10),
                    increment_port: SourcePortId(11),
                    decrement_hovered_cell: MirrorCellId(20),
                    increment_hovered_cell: MirrorCellId(21),
                    counter_sink: SinkPortId(10),
                    decrement_hovered_sink: SinkPortId(11),
                    increment_hovered_sink: SinkPortId(12),
                    initial_value: 0,
                },
            },
            wrap: wrap_lowered_program,
        },
    ],
};

struct AliasButtonPressPortConfig<'a> {
    subset: &'static str,
    binding_name: &'a str,
}

fn derive_alias_button_press_port<'a>(
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    config: &AliasButtonPressPortConfig<'_>,
) -> Result<SourcePortId, String> {
    let button = bindings.get(config.binding_name).ok_or_else(|| {
        format!(
            "{} subset requires `{}` binding",
            config.subset, config.binding_name
        )
    })?;
    let (press_port, _label_text) = lower_increment_button(button)?;
    Ok(press_port)
}

struct DerivedSourceGenericHostIrConfig<'a> {
    validation: StructuralValidationSpec<'a>,
    sink_bindings: &'a [(&'a str, SinkPortId)],
    source_binding_name: &'a str,
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    timer_source_children: &'a [TimerSourceChildConfig],
}

fn lower_derived_source_generic_host_ir<'a>(
    expressions: &'a [StaticSpannedExpression],
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    press_port: &SourcePortId,
    config: &DerivedSourceGenericHostIrConfig<'_>,
) -> Result<HostViewIr, String> {
    let source_bindings = [(config.source_binding_name, *press_port)];
    lower_generic_host_ir_from_bindings(
        expressions,
        bindings,
        &GenericHostIrSurfaceConfig {
            validation: config.validation.clone(),
            sink_bindings: config.sink_bindings,
            source_bindings: &source_bindings,
            view_site: config.view_site,
            function_instance: config.function_instance,
            timer_source_children: config.timer_source_children,
        },
    )
}

struct PressDrivenAccumulatorSemanticConfig<'a> {
    subset: &'static str,
    press_port: AliasButtonPressPortConfig<'a>,
    value_binding_name: &'static str,
    counter_sink: SinkPortId,
    hold_persistence: Option<LoweringPathPersistenceSeed<'a>>,
    shapes: &'static [SingleActionAccumulatorSemanticShape],
    program: PressDrivenAccumulatorLoweredProgramSpec,
}

impl LoweringSubset for PressDrivenAccumulatorSemanticConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.subset
    }
}

#[derive(Clone, Copy)]
enum SingleActionAccumulatorSemanticShape {
    LatestSum,
    Hold,
}

const SINGLE_ACTION_ACCUMULATOR_SEMANTIC_SHAPES: &[SingleActionAccumulatorSemanticShape] = &[
    SingleActionAccumulatorSemanticShape::LatestSum,
    SingleActionAccumulatorSemanticShape::Hold,
];

fn build_press_driven_accumulator_lowered_program(
    spec: PressDrivenAccumulatorLoweredProgramSpec,
    ir: IrProgram,
    host_view: HostViewIr,
    press_port: SourcePortId,
    counter_sink: SinkPortId,
    initial_value: i64,
    increment_delta: i64,
) -> LoweredProgram {
    match spec {
        PressDrivenAccumulatorLoweredProgramSpec::Counter => {
            LoweredProgram::Counter(CounterProgram {
                ir,
                host_view,
                press_port,
                counter_sink,
                initial_value,
                increment_delta,
            })
        }
    }
}

fn build_press_driven_accumulator_program_from_config<'a>(
    bindings: BTreeMap<String, &'a StaticSpannedExpression>,
    press_port: SourcePortId,
    host_view: HostViewIr,
    config: &PressDrivenAccumulatorSemanticConfig<'_>,
) -> Result<LoweredProgram, String> {
    let counter =
        require_top_level_binding_expr(&bindings, config.subset, config.value_binding_name)?;
    require_top_level_binding_expr(&bindings, config.subset, "document")?;

    let (initial_value, increment_delta, counter_ir) =
        lower_single_action_accumulator(&bindings, counter, press_port, config)?;

    Ok(build_press_driven_accumulator_lowered_program(
        config.program,
        counter_ir,
        host_view,
        press_port,
        config.counter_sink,
        initial_value,
        increment_delta,
    ))
}

fn derive_press_port_from_accumulator_semantic_config<'a>(
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    config: &PressDrivenAccumulatorSemanticConfig<'_>,
    _host_view: &DerivedSourceGenericHostIrConfig<'_>,
) -> Result<SourcePortId, String> {
    derive_alias_button_press_port(bindings, &config.press_port)
}

const COUNTER_BINDINGS_GENERIC_HOST_IR_GROUP: BindingsProgramGroup<
    GenericHostIrSemanticBindingsConfig<
        'static,
        SourcePortId,
        PressDrivenAccumulatorSemanticConfig<'static>,
        DerivedSourceGenericHostIrConfig<'static>,
    >,
    LoweredProgram,
> = BindingsProgramGroup {
    lower_bindings: lower_bindings_with_derived_output_owned::<
        BindingsWithGenericHostIrSemanticOutputConfig<
            'static,
            SourcePortId,
            PressDrivenAccumulatorSemanticConfig<'static>,
            DerivedSourceGenericHostIrConfig<'static>,
        >,
        (SourcePortId, HostViewIr),
        LoweredProgram,
    >,
    cases: &[BindingsProgramCase {
        source: BindingsDerivedOutputConfig {
            shared: &BindingsWithGenericHostIrSemanticOutputConfig {
                build_context: derive_press_port_from_accumulator_semantic_config,
                build_host_view: lower_derived_source_generic_host_ir,
                build_output: build_press_driven_accumulator_program_from_config,
                semantic: &PressDrivenAccumulatorSemanticConfig {
                    subset: "single_action_accumulator_document",
                    press_port: AliasButtonPressPortConfig {
                        subset: "single_action_accumulator_document",
                        binding_name: "increment_button",
                    },
                    value_binding_name: "counter",
                    counter_sink: SinkPortId(1),
                    hold_persistence: Some(LoweringPathPersistenceSeed {
                        path: &["counter"],
                        local_slot: 0,
                        persist_kind: PersistKind::Hold,
                    }),
                    shapes: SINGLE_ACTION_ACCUMULATOR_SEMANTIC_SHAPES,
                    program: PressDrivenAccumulatorLoweredProgramSpec::Counter,
                },
                host_view: &DerivedSourceGenericHostIrConfig {
                    validation: StructuralValidationSpec {
                        subset: "single_action_accumulator_document",
                        top_level_bindings: &["document", "counter", "increment_button"],
                        required_paths: &[],
                        hold_paths: &[],
                        required_functions: &[],
                        alias_paths: &[["increment_button", "event", "press"].as_slice()],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "button"].as_slice(),
                        ],
                        text_fragments: &[],
                        require_hold: false,
                        require_latest: false,
                        require_then: true,
                        require_when: false,
                        require_while: false,
                    },
                    sink_bindings: &[("counter", SinkPortId(1))],
                    source_binding_name: "increment_button",
                    view_site: ViewSiteId(1),
                    function_instance: FunctionInstanceId(1),
                    timer_source_children: &[],
                },
            },
            derive: derive_bindings_with_generic_host_ir_semantic_output,
            build_output: build_bindings_with_generic_host_ir_semantic_output,
        },
        wrap: wrap_lowered_program,
    }],
};

fn collect_path_lowering_persistence(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    path: &[&str],
    node: NodeId,
    local_slot: u32,
    persist_kind: PersistKind,
) -> Vec<IrNodePersistence> {
    persist_entry_for_path(bindings, path, node, local_slot, persist_kind)
        .into_iter()
        .collect()
}

fn collect_path_lowering_persistence_from_seed(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    seed: LoweringPathPersistenceSeed<'_>,
    node: NodeId,
) -> Vec<IrNodePersistence> {
    collect_path_lowering_persistence(
        bindings,
        seed.path,
        node,
        seed.local_slot,
        seed.persist_kind,
    )
}

pub(crate) fn collect_path_lowering_persistence_from_config(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: LoweringPathPersistenceConfig<'_>,
) -> Vec<IrNodePersistence> {
    collect_path_lowering_persistence(
        bindings,
        config.path,
        config.node,
        config.local_slot,
        config.persist_kind,
    )
}

pub(crate) fn collect_path_lowering_persistence_from_configs(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    configs: &[LoweringPathPersistenceConfig<'_>],
) -> Vec<IrNodePersistence> {
    let mut persistence = Vec::new();
    for config in configs {
        persistence.extend(collect_path_lowering_persistence_from_config(
            bindings, *config,
        ));
    }
    persistence
}

const LOWERING_PIPELINE: &[&dyn LoweringFamilyGroup] = &[
    &COUNTER_BINDINGS_GENERIC_HOST_IR_GROUP,
    &INTERVAL_SURFACE_GROUP,
    &DISPLAY_SINGLE_SINK_BINDINGS_GROUP,
    &DISPLAY_PERSISTENT_SEMANTIC_GROUP,
    &LIST_PERSISTENT_SEMANTIC_BINDINGS_GROUP,
    &GENERIC_HOST_IR_SURFACE_GROUP,
    &GENERIC_HOST_SURFACE_GROUP,
    &DISPLAY_SINK_VALUE_SURFACE_GROUP,
    &CHECKBOX_LIST_SURFACE_GROUP,
    &FORM_RUNTIME_SURFACE_GROUP,
    &LIST_HOST_ONLY_SURFACE_GROUP,
    &LIST_BOOL_TOGGLE_SEMANTIC_BINDINGS_GROUP,
    &LIST_TITLED_COLUMN_SURFACE_GROUP,
    &LIST_APPEND_LIST_SURFACE_GROUP,
    &TIMED_FLOW_SURFACE_GROUP,
];

fn lower_program_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<LoweredProgram, String> {
    let context = LoweringContext { expressions };
    let mut errors = Vec::new();

    for group in LOWERING_PIPELINE {
        match group.try_lower_group(&context) {
            LoweringAttemptOutcome::Matched(program) => return Ok(program),
            LoweringAttemptOutcome::Rejected(group_errors) => {
                errors.extend(
                    group_errors
                        .into_iter()
                        .map(|(name, error)| format!("{name}: {error}")),
                );
            }
        }
    }

    Err(format!(
        "unsupported generic lowering surface:\n- {}",
        errors.join("\n- ")
    ))
}

pub fn lower_program(source: &str) -> Result<LoweredProgram, String> {
    let expressions = parse_static_expressions(source)?;
    lower_program_from_expressions(&expressions)
}

pub fn lower_view(source: &str) -> Result<HostViewIr, String> {
    lower_program(source)?.into_host_view()
}

#[derive(Debug)]
struct ViewFunctionDef<'a> {
    parameters: Vec<String>,
    body: &'a StaticSpannedExpression,
}

#[derive(Debug)]
struct GenericViewLoweringContext<'a> {
    top_level_bindings: BTreeMap<String, &'a StaticSpannedExpression>,
    top_level_functions: BTreeMap<String, ViewFunctionDef<'a>>,
    sink_bindings: BTreeMap<String, SinkPortId>,
    press_bindings: BTreeMap<String, SourcePortId>,
}

#[derive(Debug)]
struct ViewSiteAllocator {
    next_view_site: u32,
    function_instance: FunctionInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedLabelText {
    Static(String),
    Sink(SinkPortId),
    Templated { parts: Vec<HostTemplatedTextPart> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedTextValue {
    Static(String),
    Sink(SinkPortId),
}

impl ViewSiteAllocator {
    fn new(start: ViewSiteId, function_instance: FunctionInstanceId) -> Self {
        Self {
            next_view_site: start.0,
            function_instance,
        }
    }

    fn next_key(&mut self) -> RetainedNodeKey {
        let retained_key = RetainedNodeKey {
            view_site: ViewSiteId(self.next_view_site),
            function_instance: Some(self.function_instance),
            mapped_item_identity: None,
        };
        self.next_view_site += 1;
        retained_key
    }

    fn current_view_site(&self) -> ViewSiteId {
        ViewSiteId(self.next_view_site)
    }

    fn node(&mut self, kind: HostViewKind, children: Vec<HostViewNode>) -> HostViewNode {
        HostViewNode {
            retained_key: self.next_key(),
            kind,
            children,
        }
    }

    fn node_with_key(
        &self,
        retained_key: RetainedNodeKey,
        kind: HostViewKind,
        children: Vec<HostViewNode>,
    ) -> HostViewNode {
        HostViewNode {
            retained_key,
            kind,
            children,
        }
    }
}

fn lower_generic_host_view<'a>(
    expressions: &'a [StaticSpannedExpression],
    sink_bindings: &[(&str, SinkPortId)],
    press_bindings: &[(&str, SourcePortId)],
    root_view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
) -> Result<HostViewIr, String> {
    lower_generic_host_view_with_root_binding(
        expressions,
        sink_bindings,
        press_bindings,
        root_view_site,
        function_instance,
        None,
    )
}

fn lower_generic_host_view_with_root_binding<'a>(
    expressions: &'a [StaticSpannedExpression],
    sink_bindings: &[(&str, SinkPortId)],
    press_bindings: &[(&str, SourcePortId)],
    root_view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    root_binding_name: Option<&str>,
) -> Result<HostViewIr, String> {
    lower_with_bindings(expressions, |top_level_bindings| {
        let top_level_functions = collect_top_level_view_functions(expressions);
        let context = GenericViewLoweringContext {
            top_level_bindings,
            top_level_functions,
            sink_bindings: sink_bindings
                .iter()
                .map(|(name, sink)| ((*name).to_string(), *sink))
                .collect(),
            press_bindings: press_bindings
                .iter()
                .map(|(name, port)| ((*name).to_string(), *port))
                .collect(),
        };
        let document = require_named_top_level_binding_expr(
            &context.top_level_bindings,
            "generic view lowering",
            "document",
        )?;
        let mut allocator = ViewSiteAllocator::new(root_view_site, function_instance);
        let root = lower_generic_view_expression(
            document,
            &context,
            &BTreeMap::new(),
            root_binding_name.map(str::to_string),
            &mut allocator,
        )?;
        Ok(HostViewIr { root: Some(root) })
    })
}

fn collect_top_level_view_functions<'a>(
    expressions: &'a [StaticSpannedExpression],
) -> BTreeMap<String, ViewFunctionDef<'a>> {
    expressions
        .iter()
        .filter_map(|expression| {
            let StaticExpression::Function {
                name,
                parameters,
                body,
            } = &expression.node
            else {
                return None;
            };
            Some((
                name.as_str().to_string(),
                ViewFunctionDef {
                    parameters: parameters
                        .iter()
                        .map(|parameter| parameter.node.as_str().to_string())
                        .collect(),
                    body,
                },
            ))
        })
        .collect()
}

fn lower_generic_view_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    match &expression.node {
        StaticExpression::Alias(alias) => {
            if let Ok(alias_name) = resolve_alias_binding_name(alias, env) {
                if let Some(sink) = context.sink_bindings.get(&alias_name) {
                    return Ok(allocator.node(HostViewKind::Label { sink: *sink }, Vec::new()));
                }
            }

            match alias {
                boon::parser::static_expression::Alias::WithoutPassed { parts, .. } => {
                    let alias_name = alias_path_name(parts);
                    if parts.len() == 1 {
                        let local_name = parts[0].as_str();
                        if let Some(bound) = env.get(local_name) {
                            if alias_bounces_to_same_top_level_name(bound, local_name) {
                                if let Some(sink) = context.sink_bindings.get(local_name) {
                                    return Ok(allocator
                                        .node(HostViewKind::Label { sink: *sink }, Vec::new()));
                                }
                                if let Some(binding) = context.top_level_bindings.get(local_name) {
                                    return lower_generic_view_expression(
                                        binding,
                                        context,
                                        env,
                                        Some(local_name.to_string()),
                                        allocator,
                                    );
                                }
                            }
                            return lower_generic_view_expression(
                                bound,
                                context,
                                env,
                                origin_binding.clone(),
                                allocator,
                            );
                        }
                        if let Some(sink) = context.sink_bindings.get(local_name) {
                            return Ok(
                                allocator.node(HostViewKind::Label { sink: *sink }, Vec::new())
                            );
                        }
                        if let Some(binding) = context.top_level_bindings.get(local_name) {
                            return lower_generic_view_expression(
                                binding,
                                context,
                                env,
                                Some(local_name.to_string()),
                                allocator,
                            );
                        }
                    }
                    Err(format!(
                        "generic view lowering does not know how to render alias `{alias_name}`"
                    ))
                }
                boon::parser::static_expression::Alias::WithPassed { extra_parts } => Err(format!(
                    "generic view lowering does not know how to render alias `PASSED.{}`",
                    extra_parts
                        .iter()
                        .map(|segment| segment.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                )),
            }
        }
        StaticExpression::Pipe { from, to } => match &to.node {
            StaticExpression::FunctionCall { path, arguments }
                if path_matches(path, &["Document", "new"]) && arguments.is_empty() =>
            {
                let retained_key = allocator.next_key();
                let child = match lower_generic_view_expression(
                    from,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                ) {
                    Ok(child) => child,
                    Err(error) => {
                        let Some(sink) = origin_binding
                            .as_ref()
                            .and_then(|binding| context.sink_bindings.get(binding))
                        else {
                            return Err(error);
                        };
                        allocator.node(HostViewKind::Label { sink: *sink }, Vec::new())
                    }
                };
                Ok(allocator.node_with_key(retained_key, HostViewKind::Document, vec![child]))
            }
            StaticExpression::LinkSetter { alias } => {
                let linked_binding = resolve_alias_binding_name(&alias.node, env)?;
                lower_generic_view_expression(from, context, env, Some(linked_binding), allocator)
            }
            StaticExpression::When { .. } | StaticExpression::While { .. } => {
                lower_generic_match_group_pipe(
                    from,
                    to,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                )
                .or_else(|_| {
                    lower_generic_conditional_label_pipe(
                        from,
                        to,
                        context,
                        env,
                        origin_binding.clone(),
                        allocator,
                    )
                })
            }
            StaticExpression::FunctionCall { path, arguments }
                if path.len() == 1
                    && context.top_level_functions.contains_key(path[0].as_str()) =>
            {
                lower_generic_view_function_call(
                    path[0].as_str(),
                    Some(from),
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                )
            }
            StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
                parts,
                ..
            }) if parts.len() == 1
                && context.top_level_functions.contains_key(parts[0].as_str()) =>
            {
                lower_generic_view_function_call(
                    parts[0].as_str(),
                    Some(from),
                    &[],
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                )
            }
            StaticExpression::Alias(boon::parser::static_expression::Alias::WithPassed {
                extra_parts,
            }) if extra_parts.len() == 1
                && context
                    .top_level_functions
                    .contains_key(extra_parts[0].as_str()) =>
            {
                lower_generic_view_function_call(
                    extra_parts[0].as_str(),
                    Some(from),
                    &[],
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                )
            }
            _ => Err(format!(
                "generic view lowering supports only user-function pipes at view sites, got `{:?}`",
                to.node
            )),
        },
        StaticExpression::FunctionCall { path, arguments } => {
            if path.len() == 1 && context.top_level_functions.contains_key(path[0].as_str()) {
                return lower_generic_view_function_call(
                    path[0].as_str(),
                    None,
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Document", "new"]) {
                let root = find_named_argument(arguments, "root").ok_or_else(|| {
                    "generic view lowering requires `Document/new(root: ...)`".to_string()
                })?;
                let retained_key = allocator.next_key();
                let child = lower_generic_view_expression(root, context, env, None, allocator)?;
                return Ok(allocator.node_with_key(
                    retained_key,
                    HostViewKind::Document,
                    vec![child],
                ));
            }
            if path_matches(path, &["Element", "stripe"]) {
                return lower_generic_stripe_expression(
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "stack"]) {
                return lower_generic_stack_expression(
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "container"]) {
                return lower_generic_container_expression(
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "svg"]) {
                return lower_generic_svg_expression(
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "button"]) {
                return lower_generic_button_expression(
                    expression,
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "text_input"]) {
                return lower_generic_text_input_expression(
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "slider"]) {
                return lower_generic_slider_expression(
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "select"]) {
                return lower_generic_select_expression(
                    arguments,
                    context,
                    env,
                    origin_binding.clone(),
                    allocator,
                );
            }
            if path_matches(path, &["Element", "label"]) {
                return lower_generic_label_expression(arguments, context, env, allocator);
            }
            Err(format!(
                "generic view lowering does not support call path `{}`",
                path.iter()
                    .map(|segment| segment.as_str())
                    .collect::<Vec<_>>()
                    .join("/")
            ))
        }
        StaticExpression::TextLiteral { .. }
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Number(_))
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Text(_))
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(_)) => {
            let kind = host_label_kind_from_expression(expression, context, env)?;
            Ok(allocator.node(kind, Vec::new()))
        }
        StaticExpression::Block { variables, output } => {
            let mut block_env = env.clone();
            for variable in variables {
                block_env.insert(
                    variable.node.name.as_str().to_string(),
                    &variable.node.value,
                );
            }
            lower_generic_view_expression(output, context, &block_env, origin_binding, allocator)
        }
        _ => Err(format!(
            "generic view lowering does not support expression `{:?}` at view site",
            expression.node
        )),
    }
}

fn lower_generic_view_function_call<'a>(
    function_name: &str,
    piped_input: Option<&'a StaticSpannedExpression>,
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let function = context
        .top_level_functions
        .get(function_name)
        .ok_or_else(|| format!("unknown view function `{function_name}`"))?;

    let mut child_env = BTreeMap::new();
    let mut next_parameter_index = 0usize;

    if let Some(piped_input) = piped_input {
        let parameter = function.parameters.first().ok_or_else(|| {
            format!("view function `{function_name}` does not accept a piped argument")
        })?;
        child_env.insert(parameter.clone(), piped_input);
        next_parameter_index = 1;
    }

    for argument in arguments {
        let value = argument.node.value.as_ref().ok_or_else(|| {
            format!(
                "view function `{function_name}` requires value for `{}`",
                argument.node.name
            )
        })?;
        child_env.insert(argument.node.name.as_str().to_string(), value);
    }

    for parameter in function.parameters.iter().skip(next_parameter_index) {
        if !child_env.contains_key(parameter) {
            return Err(format!(
                "view function `{function_name}` requires parameter `{parameter}`"
            ));
        }
    }

    if !child_env.contains_key("PASS") {
        if let Some(passed) = env.get("PASS").copied() {
            child_env.insert("PASS".to_string(), passed);
        }
    }

    let child_origin_binding = origin_binding.or_else(|| {
        Some(format!(
            "__{function_name}_{}",
            allocator.current_view_site().0
        ))
    });

    lower_generic_view_expression(
        function.body,
        context,
        &child_env,
        child_origin_binding,
        allocator,
    )
}

fn lower_generic_stripe_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let direction = find_named_argument(arguments, "direction")
        .map(extract_host_stripe_direction)
        .transpose()?
        .unwrap_or(HostStripeDirection::Column);
    let gap_px = find_named_argument(arguments, "gap")
        .map(extract_u32_literal)
        .transpose()?
        .unwrap_or(0);
    let padding_px = find_named_argument(arguments, "style")
        .map(extract_padding_style)
        .transpose()?
        .flatten();
    let items = find_named_argument(arguments, "items")
        .ok_or_else(|| "Element/stripe requires `items`".to_string())?;
    let StaticExpression::List { items } = &items.node else {
        return Err("Element/stripe requires LIST `items`".to_string());
    };
    let retained_key = allocator.next_key();
    let children = items
        .iter()
        .map(|item| {
            lower_generic_view_expression(item, context, env, origin_binding.clone(), allocator)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(allocator.node_with_key(
        retained_key,
        HostViewKind::StripeLayout {
            direction,
            gap_px,
            padding_px,
            width: None,
            align_cross: None,
        },
        children,
    ))
}

fn lower_generic_stack_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let style = find_named_argument(arguments, "style")
        .ok_or_else(|| "Element/stack requires `style`".to_string())?;
    let width_px = extract_required_style_u32_field(style, "width", context, env)?;
    let height_px = extract_required_style_u32_field(style, "height", context, env)?;
    let background =
        extract_required_nested_style_color_field(style, "background", "color", context, env)?
            .ok_or_else(|| "Element/stack requires style background color".to_string())?;
    let layers = find_named_argument(arguments, "layers")
        .or_else(|| find_named_argument(arguments, "items"))
        .ok_or_else(|| "Element/stack requires LIST `layers`".to_string())?;
    let StaticExpression::List { items } = &layers.node else {
        return Err("Element/stack requires LIST `layers`".to_string());
    };
    let retained_key = allocator.next_key();
    let children = items
        .iter()
        .map(|item| {
            lower_generic_view_expression(item, context, env, origin_binding.clone(), allocator)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(allocator.node_with_key(
        retained_key,
        HostViewKind::AbsolutePanel {
            width_px,
            height_px,
            background,
        },
        children,
    ))
}

fn lower_generic_container_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let child = find_named_argument(arguments, "child")
        .ok_or_else(|| "Element/container requires `child`".to_string())?;
    let child = lower_generic_view_expression(child, context, env, origin_binding, allocator)?;
    let style = find_named_argument(arguments, "style");
    let retained_key = allocator.next_key();

    if let Some(style) = style {
        if style_looks_like_positioned_box(style) {
            let width_px = extract_required_style_u32_field(style, "width", context, env)?;
            let height_px = extract_required_style_u32_field(style, "height", context, env)?;
            let x_px = extract_required_nested_style_u32_field(
                style,
                "transform",
                "move_right",
                context,
                env,
            )?;
            let y_px = extract_required_nested_style_u32_field(
                style,
                "transform",
                "move_down",
                context,
                env,
            )?;
            let padding_px = extract_optional_style_u32_field(style, "padding", context, env)?;
            let background = extract_required_nested_style_color_field(
                style,
                "background",
                "color",
                context,
                env,
            )?;
            let rounded_px =
                extract_optional_style_u32_field(style, "rounded_corners", context, env)?;
            let text_color =
                extract_required_nested_style_color_field(style, "font", "color", context, env)?;

            return Ok(allocator.node_with_key(
                retained_key,
                HostViewKind::PositionedBox {
                    x_px,
                    y_px,
                    width_px,
                    height_px,
                    padding_px,
                    background,
                    rounded_px,
                    text_color,
                },
                vec![child],
            ));
        }
    }

    let center_row = style
        .map(container_centers_row)
        .transpose()?
        .unwrap_or(false);
    Ok(allocator.node_with_key(
        retained_key,
        HostViewKind::Container { center_row },
        vec![child],
    ))
}

fn lower_generic_svg_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let click_port = origin_binding
        .as_deref()
        .and_then(|binding| context.press_bindings.get(binding).copied())
        .ok_or_else(|| {
            "generic view lowering requires sink-backed LINK for Element/svg".to_string()
        })?;
    let style = find_named_argument(arguments, "style")
        .ok_or_else(|| "Element/svg requires `style`".to_string())?;
    let width_px = extract_required_style_u32_field(style, "width", context, env)?;
    let height_px = extract_required_style_u32_field(style, "height", context, env)?;
    let background = find_object_field_expression(style, "background")?
        .map(|value| resolve_static_color_expression(value, context, env))
        .transpose()?
        .ok_or_else(|| "Element/svg requires style background color".to_string())?;
    let children = find_named_argument(arguments, "children")
        .ok_or_else(|| "Element/svg requires `children`".to_string())?;
    let child = lower_generic_svg_children_expression(children, context, env, allocator)?;
    let retained_key = allocator.next_key();

    Ok(allocator.node_with_key(
        retained_key,
        HostViewKind::AbsoluteCanvas {
            click_port,
            width_px,
            height_px,
            background,
        },
        vec![child],
    ))
}

fn lower_generic_svg_children_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(
            "generic view lowering requires `children` to be mapped `Element/svg_circle(...)`"
                .to_string(),
        );
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err(
            "generic view lowering requires `children` to be mapped `Element/svg_circle(...)`"
                .to_string(),
        );
    };
    if !path_matches(path, &["List", "map"]) {
        return Err(
            "generic view lowering requires `children` to be mapped `Element/svg_circle(...)`"
                .to_string(),
        );
    }

    let circles_sink = resolve_sink_binding_expression(from, context, env)?;
    let item_name = arguments
        .iter()
        .find(|argument| argument.node.value.is_none())
        .map(|argument| argument.node.name.as_str().to_string())
        .ok_or_else(|| "List/map requires item parameter for svg circles".to_string())?;
    let mapped_circle = find_named_argument(arguments, "new")
        .ok_or_else(|| "List/map requires `new` circle template".to_string())?;

    lower_generic_svg_circle_list(
        mapped_circle,
        &item_name,
        circles_sink,
        context,
        env,
        allocator,
    )
}

fn lower_generic_svg_circle_list<'a>(
    expression: &'a StaticSpannedExpression,
    item_name: &str,
    circles_sink: SinkPortId,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Err(
            "generic view lowering requires mapped svg children to be `Element/svg_circle(...)`"
                .to_string(),
        );
    };
    if !path_matches(path, &["Element", "svg_circle"]) {
        return Err(
            "generic view lowering requires mapped svg children to be `Element/svg_circle(...)`"
                .to_string(),
        );
    }

    let cx = find_named_argument(arguments, "cx")
        .ok_or_else(|| "Element/svg_circle requires `cx`".to_string())?;
    let cy = find_named_argument(arguments, "cy")
        .ok_or_else(|| "Element/svg_circle requires `cy`".to_string())?;
    if !alias_matches_local_field(cx, item_name, "x")
        || !alias_matches_local_field(cy, item_name, "y")
    {
        return Err(
            "generic view lowering requires svg circles to read `item.x` and `item.y`".to_string(),
        );
    }

    let radius_px = extract_required_argument_u32(arguments, "r", context, env)?;
    let style = find_named_argument(arguments, "style")
        .ok_or_else(|| "Element/svg_circle requires `style`".to_string())?;
    let fill = find_object_field_expression(style, "fill")?
        .map(|value| resolve_static_color_expression(value, context, env))
        .transpose()?
        .ok_or_else(|| "Element/svg_circle requires `fill`".to_string())?;
    let stroke = find_object_field_expression(style, "stroke")?
        .map(|value| resolve_static_color_expression(value, context, env))
        .transpose()?
        .ok_or_else(|| "Element/svg_circle requires `stroke`".to_string())?;
    let stroke_width_px = extract_required_style_u32_field(style, "stroke_width", context, env)?;

    Ok(allocator.node(
        HostViewKind::PositionedCircleList {
            circles_sink,
            radius_px,
            fill,
            stroke,
            stroke_width_px,
        },
        Vec::new(),
    ))
}

fn resolve_sink_binding_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<SinkPortId, String> {
    let StaticExpression::Alias(alias) = &expression.node else {
        return Err("generic view lowering requires sink-backed alias".to_string());
    };
    let binding_name = resolve_alias_binding_name(alias, env)?;
    context
        .sink_bindings
        .get(&binding_name)
        .copied()
        .ok_or_else(|| format!("generic view lowering does not know sink alias `{binding_name}`"))
}

fn extract_required_argument_u32<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    name: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<u32, String> {
    let value = find_named_argument(arguments, name)
        .ok_or_else(|| format!("generic view lowering requires `{name}` argument"))?;
    resolve_static_u32_expression(value, context, env)
}

fn alias_matches_local_field(
    expression: &StaticSpannedExpression,
    local_name: &str,
    field: &str,
) -> bool {
    matches!(
        &expression.node,
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 2
                && parts[0].as_str() == local_name
                && parts[1].as_str() == field
    )
}

fn lower_generic_button_expression<'a>(
    expression: &'a StaticSpannedExpression,
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let element = find_named_argument(arguments, "element")
        .ok_or_else(|| "Element/button requires `element`".to_string())?;
    ensure_button_press_link(element)?;
    let mut button_env = env.clone();
    button_env.insert("element".to_string(), element);
    let label_expression = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/button requires `label`".to_string())?;
    let label = resolve_button_label_expression(label_expression, context, &button_env)?;
    let press_port = origin_binding
        .as_deref()
        .and_then(|binding| context.press_bindings.get(binding).copied())
        .unwrap_or_else(|| SourcePortId(allocator.current_view_site().0));

    let _ = expression;
    let (
        width,
        rounded_fully,
        background,
        background_sink,
        active_background,
        outline_sink,
        active_outline,
        disabled_sink,
    ) = match find_named_argument(arguments, "style") {
        Some(style) => {
            let (
                width,
                rounded_fully,
                background,
                background_sink,
                active_background,
                outline_sink,
                active_outline,
            ) = extract_generic_button_style(
                style,
                context,
                &button_env,
                origin_binding.as_deref(),
            )?;
            (
                width,
                rounded_fully,
                background,
                background_sink,
                active_background,
                outline_sink,
                active_outline,
                extract_optional_disabled_sink(
                    style,
                    context,
                    &button_env,
                    origin_binding.as_deref(),
                )?,
            )
        }
        None => (None, false, None, None, None, None, None, None),
    };

    let kind = if width.is_some()
        || rounded_fully
        || background.is_some()
        || background_sink.is_some()
        || active_background.is_some()
        || outline_sink.is_some()
        || active_outline.is_some()
        || disabled_sink.is_some()
    {
        HostViewKind::StyledButton {
            label,
            press_port,
            disabled_sink,
            width,
            padding_px: None,
            rounded_fully,
            background,
            background_sink,
            active_background,
            outline_sink,
            active_outline,
        }
    } else {
        HostViewKind::Button {
            label,
            press_port,
            disabled_sink,
        }
    };

    Ok(allocator.node(kind, Vec::new()))
}

fn lower_generic_match_group_pipe<'a>(
    from: &'a StaticSpannedExpression,
    to: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let condition_sink = resolve_view_sink_expression(from, context, env)?;
    let arms = boolean_branch_arms(to)
        .ok_or_else(|| "generic view lowering requires WHEN/WHILE arms".to_string())?;
    let mut children = Vec::new();
    let mut match_arms = Vec::new();
    let mut fallback_child_count = 0usize;
    let mut saw_fallback = false;

    for arm in arms {
        if saw_fallback {
            return Err("generic view lowering requires wildcard match arm to be last".to_string());
        }
        let child = lower_generic_view_expression(
            &arm.body,
            context,
            env,
            origin_binding.clone(),
            allocator,
        )?;
        match pattern_to_host_view_matcher(&arm.pattern)? {
            Some(matcher) => {
                children.push(child);
                match_arms.push(HostViewMatchArm {
                    matcher,
                    child_count: 1,
                });
            }
            None => {
                saw_fallback = true;
                children.push(child);
                fallback_child_count += 1;
            }
        }
    }

    Ok(allocator.node(
        HostViewKind::MatchGroup {
            condition_sink,
            arms: match_arms,
            fallback_child_count,
        },
        children,
    ))
}

fn lower_generic_conditional_label_pipe<'a>(
    from: &'a StaticSpannedExpression,
    to: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let condition_sink =
        resolve_boolean_sink_expression(from, context, env, origin_binding.as_deref())?;
    let arms = boolean_branch_arms(to).ok_or_else(|| {
        "generic view lowering requires bool |> WHEN/WHILE for conditional labels".to_string()
    })?;
    let when_true = extract_conditional_label_arm_text(arms, "True", context, env)?;
    let when_false = extract_conditional_label_arm_text(arms, "False", context, env)?;
    Ok(allocator.node(
        HostViewKind::ConditionalLabel {
            condition_sink,
            when_true,
            when_false,
        },
        Vec::new(),
    ))
}

fn pattern_to_host_view_matcher(
    pattern: &boon::parser::static_expression::Pattern,
) -> Result<Option<HostViewMatchValue>, String> {
    match pattern {
        boon::parser::static_expression::Pattern::Literal(literal) => Ok(Some(match literal {
            boon::parser::static_expression::Literal::Text(text) => {
                HostViewMatchValue::Text(text.as_str().to_string())
            }
            boon::parser::static_expression::Literal::Tag(tag) => match tag.as_str() {
                "True" => HostViewMatchValue::Bool(true),
                "False" => HostViewMatchValue::Bool(false),
                other => HostViewMatchValue::Tag(other.to_string()),
            },
            boon::parser::static_expression::Literal::Number(_) => {
                return Err(
                    "generic view lowering does not support numeric view match arms".to_string(),
                );
            }
        })),
        boon::parser::static_expression::Pattern::WildCard => Ok(None),
        _ => Err("generic view lowering requires literal or wildcard match arms".to_string()),
    }
}

fn lower_generic_text_input_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let element = find_named_argument(arguments, "element")
        .ok_or_else(|| "Element/text_input requires `element`".to_string())?;
    ensure_change_event_link(element, "text_input")?;
    let value_sink = resolve_view_sink_expression(
        find_named_argument(arguments, "text")
            .ok_or_else(|| "Element/text_input requires `text`".to_string())?,
        context,
        env,
    )?;
    let placeholder = extract_placeholder_text(arguments, context, env)?;
    let focus_on_mount = find_named_argument(arguments, "focus")
        .map(extract_static_bool_literal)
        .transpose()?
        .unwrap_or(false);
    let change_port = resolve_required_origin_source_port(
        origin_binding.as_deref(),
        context,
        "Element/text_input",
    )?;
    let key_down_port = synthetic_aux_source_port(allocator, 50_000);
    let (width, disabled_sink) = match find_named_argument(arguments, "style") {
        Some(style) => (
            extract_optional_host_width_style(style, "width", context, env)?,
            extract_optional_disabled_sink(style, context, env, origin_binding.as_deref())?,
        ),
        None => (None, None),
    };

    let kind = if width.is_some() || disabled_sink.is_some() {
        HostViewKind::StyledTextInput {
            value_sink,
            placeholder,
            change_port,
            key_down_port,
            blur_port: None,
            focus_port: None,
            focus_on_mount,
            disabled_sink,
            width,
        }
    } else {
        HostViewKind::TextInput {
            value_sink,
            placeholder,
            change_port,
            key_down_port,
            blur_port: None,
            focus_port: None,
            focus_on_mount,
            disabled_sink: None,
        }
    };

    Ok(allocator.node(kind, Vec::new()))
}

fn lower_generic_slider_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let element = find_named_argument(arguments, "element")
        .ok_or_else(|| "Element/slider requires `element`".to_string())?;
    ensure_change_event_link(element, "slider")?;
    let value_sink = resolve_view_sink_expression(
        find_named_argument(arguments, "value")
            .ok_or_else(|| "Element/slider requires `value`".to_string())?,
        context,
        env,
    )?;
    let input_port =
        resolve_required_origin_source_port(origin_binding.as_deref(), context, "Element/slider")?;
    let min = extract_static_number_string(
        find_named_argument(arguments, "min")
            .ok_or_else(|| "Element/slider requires `min`".to_string())?,
        context,
        env,
    )?;
    let max = extract_static_number_string(
        find_named_argument(arguments, "max")
            .ok_or_else(|| "Element/slider requires `max`".to_string())?,
        context,
        env,
    )?;
    let step = extract_static_number_string(
        find_named_argument(arguments, "step")
            .ok_or_else(|| "Element/slider requires `step`".to_string())?,
        context,
        env,
    )?;
    let (width, disabled_sink) = match find_named_argument(arguments, "style") {
        Some(style) => (
            extract_optional_host_width_style(style, "width", context, env)?,
            extract_optional_disabled_sink(style, context, env, origin_binding.as_deref())?,
        ),
        None => (None, None),
    };

    let kind = if width.is_some() || disabled_sink.is_some() {
        styled_slider(
            value_sink,
            input_port,
            &min,
            &max,
            &step,
            disabled_sink,
            width,
        )
    } else {
        HostViewKind::Slider {
            value_sink,
            input_port,
            min,
            max,
            step,
            disabled_sink: None,
        }
    };

    Ok(allocator.node(kind, Vec::new()))
}

fn lower_generic_select_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<String>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let element = find_named_argument(arguments, "element")
        .ok_or_else(|| "Element/select requires `element`".to_string())?;
    ensure_change_event_link(element, "select")?;
    let selected_sink =
        resolve_select_selected_sink(arguments, context, env, origin_binding.as_deref())?;
    let change_port =
        resolve_required_origin_source_port(origin_binding.as_deref(), context, "Element/select")?;
    let options = extract_select_options(arguments, context, env)?;
    let (width, disabled_sink) = match find_named_argument(arguments, "style") {
        Some(style) => (
            extract_optional_host_width_style(style, "width", context, env)?,
            extract_optional_disabled_sink(style, context, env, origin_binding.as_deref())?,
        ),
        None => (None, None),
    };

    let kind = if width.is_some() || disabled_sink.is_some() {
        styled_select(selected_sink, change_port, options, disabled_sink, width)
    } else {
        HostViewKind::Select {
            selected_sink,
            change_port,
            options,
            disabled_sink: None,
        }
    };

    Ok(allocator.node(kind, Vec::new()))
}

fn lower_generic_label_expression<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    allocator: &mut ViewSiteAllocator,
) -> Result<HostViewNode, String> {
    let label = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/label requires `label`".to_string())?;
    let kind = host_label_kind_from_expression(label, context, env)?;
    Ok(allocator.node(kind, Vec::new()))
}

fn extract_generic_button_style<'a>(
    style: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Result<
    (
        Option<HostWidth>,
        bool,
        Option<String>,
        Option<SinkPortId>,
        Option<String>,
        Option<SinkPortId>,
        Option<String>,
    ),
    String,
> {
    let width = extract_optional_host_width_style(style, "width", context, env)?;
    let rounded_fully = match find_object_field_expression(style, "rounded_corners")? {
        Some(value) => matches!(extract_tag_name(value), Ok("Fully")),
        None => false,
    };
    let (background, background_sink, active_background) =
        extract_generic_button_background_style(style, context, env, origin_binding)?;
    let (outline_sink, active_outline) =
        extract_generic_button_outline_style(style, context, env, origin_binding)?;

    Ok((
        width,
        rounded_fully,
        background,
        background_sink,
        active_background,
        outline_sink,
        active_outline,
    ))
}

fn extract_generic_button_background_style<'a>(
    style: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Result<(Option<String>, Option<SinkPortId>, Option<String>), String> {
    let Some(background) = find_object_field_expression(style, "background")? else {
        return Ok((None, None, None));
    };
    let Some(color) = find_object_field_expression(background, "color")? else {
        return Ok((None, None, None));
    };

    if let Ok(color) = resolve_static_color_expression(color, context, env) {
        return Ok((Some(color), None, None));
    }

    if let Some(dynamic_oklch) =
        extract_dynamic_oklch_button_background(color, context, env, origin_binding)?
    {
        return Ok(dynamic_oklch);
    }

    let StaticExpression::Pipe { from, to } = &color.node else {
        return Err(
            "generic view lowering requires static color or bool |> WHEN/WHILE color style"
                .to_string(),
        );
    };
    let Some(arms) = boolean_branch_arms(to) else {
        return Err(
            "generic view lowering requires static color or bool |> WHEN/WHILE color style"
                .to_string(),
        );
    };
    let background_sink = resolve_boolean_sink_expression(from, context, env, origin_binding)?;
    let false_color = extract_when_color_arm(arms, "False", context, env)?;
    let true_color = extract_when_color_arm(arms, "True", context, env)?;
    Ok((Some(false_color), Some(background_sink), Some(true_color)))
}

fn extract_dynamic_oklch_button_background<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Result<Option<(Option<String>, Option<SinkPortId>, Option<String>)>, String> {
    let StaticExpression::TaggedObject { tag, object } = &expression.node else {
        return Ok(None);
    };
    if tag.as_str() != "Oklch" {
        return Ok(None);
    }

    let Some(lightness) = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "lightness")
        .map(|variable| &variable.node.value)
    else {
        return Ok(None);
    };
    let StaticExpression::Pipe { from, to } = &lightness.node else {
        return Ok(None);
    };
    let Some(arms) = boolean_branch_arms(to) else {
        return Ok(None);
    };

    let background_sink = resolve_boolean_sink_expression(from, context, env, origin_binding)?;
    let false_lightness = extract_when_number_arm(arms, "False", context, env)?;
    let true_lightness = extract_when_number_arm(arms, "True", context, env)?;
    let chroma = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "chroma")
        .map(|variable| resolve_static_number_expression(&variable.node.value, context, env))
        .transpose()?
        .unwrap_or(0.0);
    let hue = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "hue")
        .map(|variable| resolve_static_number_expression(&variable.node.value, context, env))
        .transpose()?
        .unwrap_or(0.0);
    let alpha = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "alpha")
        .map(|variable| resolve_static_number_expression(&variable.node.value, context, env))
        .transpose()?;

    let format_color = |lightness: f64| match alpha {
        Some(alpha) => format!("oklch({lightness} {chroma} {hue} / {alpha})"),
        None => format!("oklch({lightness} {chroma} {hue})"),
    };

    Ok(Some((
        Some(format_color(false_lightness)),
        Some(background_sink),
        Some(format_color(true_lightness)),
    )))
}

fn extract_generic_button_outline_style<'a>(
    style: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Result<(Option<SinkPortId>, Option<String>), String> {
    let Some(outline) = find_object_field_expression(style, "outline")? else {
        return Ok((None, None));
    };
    let StaticExpression::Pipe { from, to } = &outline.node else {
        return Err("generic view lowering requires bool |> WHEN/WHILE outline style".to_string());
    };
    let Some(arms) = boolean_branch_arms(to) else {
        return Err("generic view lowering requires bool |> WHEN/WHILE outline style".to_string());
    };
    let outline_sink = resolve_boolean_sink_expression(from, context, env, origin_binding)?;
    let active_outline = extract_when_outline_arm(arms, "True", context, env)?;
    if extract_when_outline_arm(arms, "False", context, env)?.is_some() {
        return Err(
            "generic view lowering requires `False => NoOutline` in outline style".to_string(),
        );
    }
    Ok((Some(outline_sink), active_outline))
}

fn boolean_branch_arms(
    expression: &StaticSpannedExpression,
) -> Option<&[boon::parser::static_expression::Arm]> {
    match &expression.node {
        StaticExpression::When { arms } | StaticExpression::While { arms } => Some(arms),
        _ => None,
    }
}

fn resolve_boolean_sink_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Result<SinkPortId, String> {
    let StaticExpression::Alias(alias) = &expression.node else {
        return Err(
            "generic view lowering requires sink-backed boolean alias in style".to_string(),
        );
    };

    if let boon::parser::static_expression::Alias::WithoutPassed { parts, .. } = alias {
        if parts.len() == 1 {
            let local_name = parts[0].as_str();
            if let Some(sink) = context.sink_bindings.get(local_name) {
                return Ok(*sink);
            }
        }
    }

    if let Ok(binding_name) = resolve_alias_binding_name(alias, env) {
        if let Some(sink) = context.sink_bindings.get(&binding_name) {
            return Ok(*sink);
        }
    }

    if let Some(sink) = resolve_link_field_sink_expression(alias, context, env, origin_binding) {
        return Ok(sink);
    }

    if let Some(sink) =
        resolve_origin_boolean_style_sink_expression(expression, context, env, origin_binding)
    {
        return Ok(sink);
    }

    Err("generic view lowering requires sink-backed boolean alias in style".to_string())
}

fn resolve_origin_boolean_style_sink_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Option<SinkPortId> {
    let origin_binding = origin_binding?;
    let StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
        parts,
        ..
    }) = &expression.node
    else {
        return None;
    };
    if parts.len() != 1 {
        return None;
    }

    let bound = env.get(parts[0].as_str())?;
    if !is_alias_to_static_comparator(bound, env) {
        return None;
    }

    context
        .sink_bindings
        .get(&format!("{origin_binding}.active"))
        .copied()
        .or_else(|| {
            context
                .sink_bindings
                .get(&format!("{origin_binding}.selected"))
                .copied()
        })
}

fn is_alias_to_static_comparator<'a>(
    expression: &'a StaticSpannedExpression,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> bool {
    match &expression.node {
        StaticExpression::Comparator(boon::parser::static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        })
        | StaticExpression::Comparator(boon::parser::static_expression::Comparator::NotEqual {
            operand_a,
            operand_b,
        }) => {
            (is_resolved_alias_expression(operand_a, env)
                && is_static_comparator_operand(operand_b))
                || (is_resolved_alias_expression(operand_b, env)
                    && is_static_comparator_operand(operand_a))
        }
        _ => false,
    }
}

fn is_resolved_alias_expression<'a>(
    expression: &'a StaticSpannedExpression,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> bool {
    match &expression.node {
        StaticExpression::Alias(alias) => resolve_alias_binding_name(alias, env).is_ok(),
        _ => false,
    }
}

fn is_static_comparator_operand(expression: &StaticSpannedExpression) -> bool {
    match &expression.node {
        StaticExpression::TextLiteral { .. } => true,
        StaticExpression::Literal(_) => true,
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) => parts.len() == 1,
        _ => false,
    }
}

fn resolve_link_field_sink_expression<'a>(
    alias: &boon::parser::static_expression::Alias,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Option<SinkPortId> {
    let origin_binding = origin_binding?;
    let boon::parser::static_expression::Alias::WithoutPassed { parts, .. } = alias else {
        return None;
    };
    if parts.len() != 2 {
        return None;
    }
    let local_name = parts[0].as_str();
    let field = parts[1].as_str();
    let bound = env.get(local_name)?;
    let field_expression = find_object_field_expression(bound, field).ok()??;
    if !matches!(field_expression.node, StaticExpression::Link) {
        return None;
    }
    context
        .sink_bindings
        .get(&format!("{origin_binding}.{field}"))
        .copied()
}

fn extract_when_color_arm(
    arms: &[boon::parser::static_expression::Arm],
    expected_tag: &str,
    context: &GenericViewLoweringContext<'_>,
    env: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<String, String> {
    let arm = arms
        .iter()
        .find(|arm| {
            matches!(arm.pattern, boon::parser::static_expression::Pattern::Literal(
            boon::parser::static_expression::Literal::Tag(ref tag)
        ) if tag.as_str() == expected_tag)
        })
        .ok_or_else(|| {
            format!("generic view lowering requires `{expected_tag}` WHEN arm in color style")
        })?;
    resolve_static_color_expression(&arm.body, context, env)
}

fn extract_when_number_arm(
    arms: &[boon::parser::static_expression::Arm],
    expected_tag: &str,
    context: &GenericViewLoweringContext<'_>,
    env: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<f64, String> {
    let arm = arms
        .iter()
        .find(|arm| {
            matches!(arm.pattern, boon::parser::static_expression::Pattern::Literal(
            boon::parser::static_expression::Literal::Tag(ref tag)
        ) if tag.as_str() == expected_tag)
        })
        .ok_or_else(|| {
            format!("generic view lowering requires `{expected_tag}` WHEN arm in numeric style")
        })?;
    resolve_static_number_expression(&arm.body, context, env)
}

fn extract_when_outline_arm(
    arms: &[boon::parser::static_expression::Arm],
    expected_tag: &str,
    context: &GenericViewLoweringContext<'_>,
    env: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<Option<String>, String> {
    let arm = arms
        .iter()
        .find(|arm| {
            matches!(arm.pattern, boon::parser::static_expression::Pattern::Literal(
            boon::parser::static_expression::Literal::Tag(ref tag)
        ) if tag.as_str() == expected_tag)
        })
        .ok_or_else(|| {
            format!("generic view lowering requires `{expected_tag}` WHEN arm in outline style")
        })?;

    if matches!(extract_tag_name(&arm.body), Ok("NoOutline")) {
        return Ok(None);
    }

    let thickness = extract_required_style_u32_field(&arm.body, "thickness", context, env)?;
    let color = find_object_field_expression(&arm.body, "color")?
        .ok_or_else(|| "generic view lowering requires outline color".to_string())
        .and_then(|color| resolve_static_color_expression(color, context, env))?;
    Ok(Some(format!("{thickness}px solid {color}")))
}

fn extract_conditional_label_arm_text<'a>(
    arms: &'a [boon::parser::static_expression::Arm],
    expected_tag: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<String, String> {
    let arm = arms
        .iter()
        .find(|arm| {
            matches!(arm.pattern, boon::parser::static_expression::Pattern::Literal(
                boon::parser::static_expression::Literal::Tag(ref tag)
            ) if tag.as_str() == expected_tag)
        })
        .ok_or_else(|| {
            format!("generic view lowering requires `{expected_tag}` arm in conditional label")
        })?;

    let StaticExpression::FunctionCall { path, arguments } = &arm.body.node else {
        return Err(
            "generic view lowering requires conditional label arms to return `Element/label(...)`"
                .to_string(),
        );
    };
    if !path_matches(path, &["Element", "label"]) {
        return Err(
            "generic view lowering requires conditional label arms to return `Element/label(...)`"
                .to_string(),
        );
    }
    let label = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/label requires `label`".to_string())?;
    resolve_static_text_expression(label, context, env)
}

fn host_label_kind_from_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<HostViewKind, String> {
    match resolve_label_text_expression(expression, context, env)? {
        ResolvedLabelText::Static(text) => Ok(HostViewKind::StaticLabel { text }),
        ResolvedLabelText::Sink(sink) => Ok(HostViewKind::Label { sink }),
        ResolvedLabelText::Templated { parts } => Ok(HostViewKind::TemplatedLabel { parts }),
    }
}

fn resolve_button_label_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<HostButtonLabel, String> {
    match resolve_label_text_expression(expression, context, env)? {
        ResolvedLabelText::Static(text) => Ok(HostButtonLabel::Static(text)),
        ResolvedLabelText::Sink(sink) => Ok(HostButtonLabel::Sink(sink)),
        ResolvedLabelText::Templated { parts } => Ok(HostButtonLabel::Templated(parts)),
    }
}

fn resolve_label_text_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<ResolvedLabelText, String> {
    match resolve_text_value(expression, context, env) {
        Ok(ResolvedTextValue::Static(text)) => Ok(ResolvedLabelText::Static(text)),
        Ok(ResolvedTextValue::Sink(sink)) => Ok(ResolvedLabelText::Sink(sink)),
        Err(_) => match &expression.node {
            StaticExpression::TextLiteral { parts, .. } => {
                let mut fragments = Vec::new();
                let mut current_static = String::new();

                for part in parts {
                    match part {
                        boon::parser::static_expression::TextPart::Text(text) => {
                            current_static.push_str(text.as_str());
                        }
                        boon::parser::static_expression::TextPart::Interpolation {
                            var, ..
                        } => match resolve_text_interpolation(var.as_str(), context, env)? {
                            ResolvedTextValue::Static(text) => {
                                current_static.push_str(&text);
                            }
                            ResolvedTextValue::Sink(next_sink) => {
                                if !current_static.is_empty() {
                                    fragments.push(HostTemplatedTextPart::Static(std::mem::take(
                                        &mut current_static,
                                    )));
                                }
                                fragments.push(HostTemplatedTextPart::Sink(next_sink));
                            }
                        },
                    }
                }

                if !current_static.is_empty() {
                    fragments.push(HostTemplatedTextPart::Static(current_static));
                }

                match fragments.as_slice() {
                    [HostTemplatedTextPart::Sink(sink)] => Ok(ResolvedLabelText::Sink(*sink)),
                    [] => Ok(ResolvedLabelText::Static(String::new())),
                    [HostTemplatedTextPart::Static(text)] => {
                        Ok(ResolvedLabelText::Static(text.clone()))
                    }
                    _ => Ok(ResolvedLabelText::Templated { parts: fragments }),
                }
            }
            _ => Err("generic view lowering could not resolve label text".to_string()),
        },
    }
}

fn resolve_text_value<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<ResolvedTextValue, String> {
    match &expression.node {
        StaticExpression::Alias(alias) => resolve_alias_text_value(alias, context, env),
        StaticExpression::TextLiteral { parts, .. } => {
            if parts
                .iter()
                .all(|part| matches!(part, boon::parser::static_expression::TextPart::Text(_)))
            {
                let mut text = String::new();
                for part in parts {
                    let boon::parser::static_expression::TextPart::Text(fragment) = part else {
                        unreachable!();
                    };
                    text.push_str(fragment.as_str());
                }
                Ok(ResolvedTextValue::Static(text))
            } else {
                Err("dynamic TEXT requires templated label lowering".to_string())
            }
        }
        StaticExpression::Literal(boon::parser::static_expression::Literal::Number(number)) => {
            Ok(ResolvedTextValue::Static(number.to_string()))
        }
        StaticExpression::Literal(boon::parser::static_expression::Literal::Text(text))
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(text)) => {
            Ok(ResolvedTextValue::Static(text.as_str().to_string()))
        }
        _ => Err("generic view lowering could not resolve text value".to_string()),
    }
}

fn resolve_alias_text_value<'a>(
    alias: &boon::parser::static_expression::Alias,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<ResolvedTextValue, String> {
    if let Ok(alias_name) = resolve_alias_binding_name(alias, env) {
        if let Some(sink) = context.sink_bindings.get(&alias_name) {
            return Ok(ResolvedTextValue::Sink(*sink));
        }
    }

    match alias {
        boon::parser::static_expression::Alias::WithoutPassed { parts, .. } => {
            let alias_name = alias_path_name(parts);
            if parts.len() == 1 {
                let local_name = parts[0].as_str();
                if let Some(sink) = context.sink_bindings.get(local_name) {
                    return Ok(ResolvedTextValue::Sink(*sink));
                }
                if let Some(bound) = env.get(local_name) {
                    if alias_bounces_to_same_top_level_name(bound, local_name) {
                        if let Some(sink) = context.sink_bindings.get(local_name) {
                            return Ok(ResolvedTextValue::Sink(*sink));
                        }
                        if let Some(binding) = context.top_level_bindings.get(local_name) {
                            return resolve_text_value(binding, context, env);
                        }
                    }
                    return resolve_text_value(bound, context, env);
                }
                if let Some(sink) = context.sink_bindings.get(local_name) {
                    return Ok(ResolvedTextValue::Sink(*sink));
                }
                if let Some(binding) = context.top_level_bindings.get(local_name) {
                    return resolve_text_value(binding, context, env);
                }
            }
            if let Some(binding) = binding_at_path(
                &context.top_level_bindings,
                &parts.iter().map(|part| part.as_str()).collect::<Vec<_>>(),
            ) {
                return resolve_text_value(binding, context, env);
            }
            Err(format!(
                "generic view lowering does not know text alias `{alias_name}`"
            ))
        }
        boon::parser::static_expression::Alias::WithPassed { extra_parts } => Err(format!(
            "generic view lowering does not know text alias `PASSED.{}`",
            extra_parts
                .iter()
                .map(|segment| segment.as_str())
                .collect::<Vec<_>>()
                .join(".")
        )),
    }
}

fn resolve_alias_binding_name<'a>(
    alias: &boon::parser::static_expression::Alias,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<String, String> {
    match alias {
        boon::parser::static_expression::Alias::WithoutPassed { parts, .. } => {
            if parts.len() == 1 {
                let local_name = parts[0].as_str();
                if let Some(bound) = env.get(local_name) {
                    if alias_bounces_to_same_top_level_name(bound, local_name) {
                        return Ok(local_name.to_string());
                    }
                    return resolve_expression_binding_name(bound, env);
                }
            }
            Ok(alias_path_name(parts))
        }
        boon::parser::static_expression::Alias::WithPassed { extra_parts } => {
            let passed = env.get("PASS").ok_or_else(|| {
                "generic view lowering requires `PASS:` binding for `PASSED...` aliases".to_string()
            })?;
            let mut current = *passed;
            let mut next_index = 0usize;

            while next_index < extra_parts.len() {
                match &current.node {
                    StaticExpression::Object(object)
                    | StaticExpression::TaggedObject { object, .. } => {
                        let field = extra_parts[next_index].as_str();
                        current = object
                            .variables
                            .iter()
                            .find(|variable| variable.node.name.as_str() == field)
                            .map(|variable| &variable.node.value)
                            .ok_or_else(|| {
                                format!(
                                    "generic view lowering could not resolve `PASSED.{}`",
                                    extra_parts
                                        .iter()
                                        .map(|segment| segment.as_str())
                                        .collect::<Vec<_>>()
                                        .join(".")
                                )
                            })?;
                        next_index += 1;
                    }
                    _ => break,
                }
            }

            let mut binding = match &current.node {
                StaticExpression::Alias(
                    boon::parser::static_expression::Alias::WithoutPassed { parts, .. },
                ) if parts.len() == 1 => {
                    let local_name = parts[0].as_str();
                    let self_cycles_through_pass = matches!(
                        env.get(local_name).map(|bound| &bound.node),
                        Some(StaticExpression::Alias(
                            boon::parser::static_expression::Alias::WithPassed { extra_parts }
                        )) if extra_parts.len() == 1 && extra_parts[0].as_str() == local_name
                    );
                    if self_cycles_through_pass {
                        local_name.to_string()
                    } else {
                        resolve_expression_binding_name(current, env)?
                    }
                }
                StaticExpression::Alias(boon::parser::static_expression::Alias::WithPassed {
                    extra_parts,
                }) => extra_parts
                    .iter()
                    .map(|segment| segment.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                _ => resolve_expression_binding_name(current, env)?,
            };
            if next_index < extra_parts.len() {
                if !binding.is_empty() {
                    binding.push('.');
                }
                binding.push_str(
                    &extra_parts[next_index..]
                        .iter()
                        .map(|segment| segment.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
                );
            }
            Ok(binding)
        }
    }
}

fn resolve_expression_binding_name<'a>(
    expression: &'a StaticSpannedExpression,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<String, String> {
    match &expression.node {
        StaticExpression::Alias(alias) => resolve_alias_binding_name(alias, env),
        _ => Err("generic view lowering requires alias-based binding identity".to_string()),
    }
}

fn resolve_text_interpolation<'a>(
    variable: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<ResolvedTextValue, String> {
    if let Some(bound) = env.get(variable) {
        return resolve_text_value(bound, context, env);
    }
    if let Some(sink) = context.sink_bindings.get(variable) {
        return Ok(ResolvedTextValue::Sink(*sink));
    }
    if let Some(binding) = context.top_level_bindings.get(variable) {
        return resolve_text_value(binding, context, env);
    }
    let path = variable.split('.').collect::<Vec<_>>();
    if path.len() > 1 {
        if let Some(binding) = resolve_generic_binding_at_path(context, env, &path) {
            return resolve_text_value(binding, context, env);
        }
    }
    Err(format!(
        "generic view lowering does not know interpolated variable `{variable}`"
    ))
}

fn resolve_static_text_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<String, String> {
    if let StaticExpression::FunctionCall { path, arguments } = &expression.node {
        if path.len() == 1 {
            if let Some(function) = context.top_level_functions.get(path[0].as_str()) {
                let mut function_env = BTreeMap::new();
                for argument in arguments {
                    let value = argument.node.value.as_ref().ok_or_else(|| {
                        "generic view lowering requires named arguments for text functions"
                            .to_string()
                    })?;
                    function_env.insert(argument.node.name.as_str().to_string(), value);
                }
                return resolve_static_text_expression(function.body, context, &function_env);
            }
        }
    }

    if let StaticExpression::TextLiteral { parts, .. } = &expression.node {
        let mut text = String::new();
        for part in parts {
            match part {
                boon::parser::static_expression::TextPart::Text(fragment) => {
                    text.push_str(fragment.as_str());
                }
                boon::parser::static_expression::TextPart::Interpolation { var, .. } => {
                    match resolve_text_interpolation(var.as_str(), context, env)? {
                        ResolvedTextValue::Static(value) => text.push_str(&value),
                        ResolvedTextValue::Sink(_) => {
                            return Err(
                                "generic view lowering requires static button labels for the current subset"
                                    .to_string(),
                            )
                        }
                    }
                }
            }
        }
        return Ok(text);
    }

    match resolve_text_value(expression, context, env)? {
        ResolvedTextValue::Static(text) => Ok(text),
        ResolvedTextValue::Sink(_) => Err(
            "generic view lowering requires static button labels for the current subset"
                .to_string(),
        ),
    }
}

fn extract_host_stripe_direction(
    expression: &StaticSpannedExpression,
) -> Result<HostStripeDirection, String> {
    match extract_tag_name(expression)? {
        "Row" => Ok(HostStripeDirection::Row),
        "Column" => Ok(HostStripeDirection::Column),
        other => Err(format!("unsupported stripe direction `{other}`")),
    }
}

fn extract_padding_style(expression: &StaticSpannedExpression) -> Result<Option<u32>, String> {
    let StaticExpression::Object(object) = &expression.node else {
        return Err("style argument must be object".to_string());
    };
    match object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "padding")
    {
        Some(variable) => extract_u32_literal(&variable.node.value).map(Some),
        None => Ok(None),
    }
}

fn container_centers_row(expression: &StaticSpannedExpression) -> Result<bool, String> {
    let Some(align) = object_variables(expression)?
        .iter()
        .find(|variable| variable.node.name.as_str() == "align")
    else {
        return Ok(false);
    };
    let StaticExpression::Object(align_object) = &align.node.value.node else {
        return Err("container align style must be object".to_string());
    };
    Ok(align_object.variables.iter().any(|variable| {
        variable.node.name.as_str() == "row"
            && matches!(extract_tag_name(&variable.node.value), Ok("Center"))
    }))
}

fn object_variables(
    expression: &StaticSpannedExpression,
) -> Result<
    &[boon::parser::static_expression::Spanned<boon::parser::static_expression::Variable>],
    String,
> {
    match &expression.node {
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => {
            Ok(&object.variables)
        }
        _ => Err("style argument must be object".to_string()),
    }
}

fn find_object_field_expression<'a>(
    expression: &'a StaticSpannedExpression,
    field: &str,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    Ok(object_variables(expression)?
        .iter()
        .find(|variable| variable.node.name.as_str() == field)
        .map(|variable| &variable.node.value))
}

fn style_looks_like_positioned_box(style: &StaticSpannedExpression) -> bool {
    match find_object_field_expression(style, "transform") {
        Ok(Some(_)) => true,
        _ => false,
    }
}

fn resolve_static_number_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<f64, String> {
    match &expression.node {
        StaticExpression::Literal(boon::parser::static_expression::Literal::Number(number)) => {
            Ok(*number)
        }
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) => {
            if parts.len() == 1 {
                let local_name = parts[0].as_str();
                if let Some(bound) = env.get(local_name) {
                    return resolve_static_number_expression(bound, context, env);
                }
                if let Some(binding) = context.top_level_bindings.get(local_name) {
                    return resolve_static_number_expression(binding, context, env);
                }
                return Err(format!(
                    "generic view lowering does not know numeric alias `{local_name}`"
                ));
            }
            if let Some(binding) = binding_at_path(
                &context.top_level_bindings,
                &parts.iter().map(|part| part.as_str()).collect::<Vec<_>>(),
            ) {
                return resolve_static_number_expression(binding, context, env);
            }
            if let Some(binding) = resolve_generic_binding_at_path(
                context,
                env,
                &parts.iter().map(|part| part.as_str()).collect::<Vec<_>>(),
            ) {
                return resolve_static_number_expression(binding, context, env);
            }
            Err(format!(
                "generic view lowering does not know numeric alias `{}`",
                alias_path_name(parts)
            ))
        }
        _ => Err("generic view lowering requires a static numeric style value".to_string()),
    }
}

fn resolve_generic_binding_at_path<'a>(
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    path: &[&str],
) -> Option<&'a StaticSpannedExpression> {
    let (root, fields) = path.split_first()?;
    let mut expression = env
        .get(*root)
        .copied()
        .or_else(|| context.top_level_bindings.get(*root).copied())?;
    let mut current_env = env.clone();
    for field in fields {
        let (next_expression, next_env) =
            resolve_generic_expression_field(expression, field, context, &current_env)?;
        expression = next_expression;
        current_env = next_env;
    }
    loop {
        let StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) = &expression.node
        else {
            break;
        };
        if parts.len() != 1 {
            break;
        }
        let local_name = parts[0].as_str();
        if let Some(bound) = current_env.get(local_name).copied() {
            if alias_bounces_to_same_top_level_name(bound, local_name) {
                break;
            }
            expression = bound;
            continue;
        }
        if let Some(bound) = context.top_level_bindings.get(local_name).copied() {
            if alias_bounces_to_same_top_level_name(bound, local_name) {
                break;
            }
            expression = bound;
            continue;
        }
        break;
    }
    Some(expression)
}

fn resolve_generic_expression_field<'a>(
    expression: &'a StaticSpannedExpression,
    field: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Option<(
    &'a StaticSpannedExpression,
    BTreeMap<String, &'a StaticSpannedExpression>,
)> {
    match &expression.node {
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .find(|variable| variable.node.name.as_str() == field)
            .map(|variable| (&variable.node.value, env.clone())),
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) if parts.len() == 1 => {
            let local_name = parts[0].as_str();
            let bound = env
                .get(local_name)
                .copied()
                .or_else(|| context.top_level_bindings.get(local_name).copied())?;
            resolve_generic_expression_field(bound, field, context, env)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.top_level_functions.contains_key(path[0].as_str()) =>
        {
            let function = context.top_level_functions.get(path[0].as_str())?;
            let mut function_env = BTreeMap::new();
            for argument in arguments {
                let value = argument.node.value.as_ref()?;
                function_env.insert(argument.node.name.as_str().to_string(), value);
            }
            resolve_generic_expression_field(function.body, field, context, &function_env)
        }
        _ => None,
    }
}

fn resolve_static_u32_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<u32, String> {
    let number = resolve_static_number_expression(expression, context, env)?;
    if number < 0.0 || number.fract() != 0.0 {
        return Err(
            "generic view lowering requires a non-negative integer style value".to_string(),
        );
    }
    Ok(number as u32)
}

fn extract_optional_host_width_style<'a>(
    style: &'a StaticSpannedExpression,
    field: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<Option<HostWidth>, String> {
    let Some(value) = find_object_field_expression(style, field)? else {
        return Ok(None);
    };
    match &value.node {
        StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Text(tag))
            if tag.as_str() == "Fill" =>
        {
            Ok(Some(HostWidth::Fill))
        }
        _ => resolve_static_u32_expression(value, context, env).map(|px| Some(HostWidth::Px(px))),
    }
}

fn extract_required_style_u32_field<'a>(
    style: &'a StaticSpannedExpression,
    field: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<u32, String> {
    let value = find_object_field_expression(style, field)?
        .ok_or_else(|| format!("style requires `{field}`"))?;
    resolve_static_u32_expression(value, context, env)
}

fn extract_optional_style_u32_field<'a>(
    style: &'a StaticSpannedExpression,
    field: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<Option<u32>, String> {
    let Some(value) = find_object_field_expression(style, field)? else {
        return Ok(None);
    };
    resolve_static_u32_expression(value, context, env).map(Some)
}

fn extract_required_nested_style_u32_field<'a>(
    style: &'a StaticSpannedExpression,
    parent_field: &str,
    field: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<u32, String> {
    let parent = find_object_field_expression(style, parent_field)?
        .ok_or_else(|| format!("style requires `{parent_field}`"))?;
    let value = find_object_field_expression(parent, field)?
        .ok_or_else(|| format!("style requires `{field}` inside `{parent_field}`"))?;
    resolve_static_u32_expression(value, context, env)
}

fn extract_required_nested_style_color_field<'a>(
    style: &'a StaticSpannedExpression,
    parent: &str,
    field: &str,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<Option<String>, String> {
    let Some(parent) = find_object_field_expression(style, parent)? else {
        return Ok(None);
    };
    let Some(value) = find_object_field_expression(parent, field)? else {
        return Ok(None);
    };
    resolve_static_color_expression(value, context, env).map(Some)
}

fn resolve_static_color_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<String, String> {
    match &expression.node {
        StaticExpression::TextLiteral { .. } => {
            resolve_static_text_expression(expression, context, env)
        }
        StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(tag)) => {
            match tag.as_str() {
                "White" => Ok("white".to_string()),
                other => Ok(other.to_ascii_lowercase()),
            }
        }
        StaticExpression::Literal(boon::parser::static_expression::Literal::Text(text)) => {
            Ok(text.as_str().to_string())
        }
        StaticExpression::TaggedObject { tag, object } if tag.as_str() == "Oklch" => {
            let lightness = object
                .variables
                .iter()
                .find(|variable| variable.node.name.as_str() == "lightness")
                .ok_or_else(|| "Oklch color requires `lightness`".to_string())
                .and_then(|variable| {
                    resolve_static_number_expression(&variable.node.value, context, env)
                })?;
            let chroma = object
                .variables
                .iter()
                .find(|variable| variable.node.name.as_str() == "chroma")
                .map(|variable| {
                    resolve_static_number_expression(&variable.node.value, context, env)
                })
                .transpose()?
                .unwrap_or(0.0);
            let hue = object
                .variables
                .iter()
                .find(|variable| variable.node.name.as_str() == "hue")
                .map(|variable| {
                    resolve_static_number_expression(&variable.node.value, context, env)
                })
                .transpose()?
                .unwrap_or(0.0);
            let alpha = object
                .variables
                .iter()
                .find(|variable| variable.node.name.as_str() == "alpha")
                .map(|variable| {
                    resolve_static_number_expression(&variable.node.value, context, env)
                })
                .transpose()?;

            match alpha {
                Some(alpha) => Ok(format!("oklch({lightness} {chroma} {hue} / {alpha})")),
                None => Ok(format!("oklch({lightness} {chroma} {hue})")),
            }
        }
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) => {
            if parts.len() == 1 {
                let local_name = parts[0].as_str();
                if let Some(bound) = env.get(local_name) {
                    return resolve_static_color_expression(bound, context, env);
                }
                if let Some(binding) = context.top_level_bindings.get(local_name) {
                    return resolve_static_color_expression(binding, context, env);
                }
                return Err(format!(
                    "generic view lowering does not know color alias `{local_name}`"
                ));
            }
            if let Some(binding) = binding_at_path(
                &context.top_level_bindings,
                &parts.iter().map(|part| part.as_str()).collect::<Vec<_>>(),
            ) {
                return resolve_static_color_expression(binding, context, env);
            }
            Err(format!(
                "generic view lowering does not know color alias `{}`",
                alias_path_name(parts)
            ))
        }
        _ => Err("generic view lowering requires a static color style value".to_string()),
    }
}

fn extract_tag_name(expression: &StaticSpannedExpression) -> Result<&str, String> {
    match &expression.node {
        StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Text(tag)) => {
            Ok(tag.as_str())
        }
        _ => Err("expected tag/text literal".to_string()),
    }
}

fn extract_u32_literal(expression: &StaticSpannedExpression) -> Result<u32, String> {
    let number = extract_number_literal(expression)?;
    if number < 0.0 || number.fract() != 0.0 {
        return Err("expected non-negative integer numeric literal".to_string());
    }
    Ok(number as u32)
}

fn ensure_button_press_link(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Object(object) = &expression.node else {
        return Err("button element must be object".to_string());
    };
    let event_var = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "event")
        .ok_or_else(|| "button element requires event object".to_string())?;
    let StaticExpression::Object(event_object) = &event_var.node.value.node else {
        return Err("button event must be object".to_string());
    };
    let press_var = event_object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "press")
        .ok_or_else(|| "button event object requires press".to_string())?;
    if matches!(press_var.node.value.node, StaticExpression::Link) {
        Ok(())
    } else {
        Err("button press must be LINK".to_string())
    }
}

fn ensure_change_event_link(
    expression: &StaticSpannedExpression,
    element_name: &str,
) -> Result<(), String> {
    let StaticExpression::Object(object) = &expression.node else {
        return Err(format!("{element_name} element must be object"));
    };
    let event_var = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "event")
        .ok_or_else(|| format!("{element_name} element requires event object"))?;
    let StaticExpression::Object(event_object) = &event_var.node.value.node else {
        return Err(format!("{element_name} event must be object"));
    };
    let change_var = event_object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "change")
        .ok_or_else(|| format!("{element_name} event object requires change"))?;
    if matches!(change_var.node.value.node, StaticExpression::Link) {
        Ok(())
    } else {
        Err(format!("{element_name} change must be LINK"))
    }
}

fn resolve_required_origin_source_port<'a>(
    origin_binding: Option<&str>,
    context: &GenericViewLoweringContext<'a>,
    element_name: &str,
) -> Result<SourcePortId, String> {
    let origin_binding = origin_binding.ok_or_else(|| {
        format!("generic view lowering requires sink-backed LINK for {element_name}")
    })?;
    context
        .press_bindings
        .get(origin_binding)
        .copied()
        .ok_or_else(|| {
            format!("generic view lowering does not know source alias `{origin_binding}`")
        })
}

fn synthetic_aux_source_port(allocator: &ViewSiteAllocator, base: u32) -> SourcePortId {
    SourcePortId(base.saturating_add(allocator.current_view_site().0))
}

fn resolve_view_sink_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<SinkPortId, String> {
    let StaticExpression::Alias(alias) = &expression.node else {
        return Err("generic view lowering requires sink-backed alias".to_string());
    };

    if let boon::parser::static_expression::Alias::WithoutPassed { parts, .. } = alias {
        if parts.len() == 1 {
            let local_name = parts[0].as_str();
            if let Some(sink) = context.sink_bindings.get(local_name) {
                return Ok(*sink);
            }
        }
    }

    let binding_name = resolve_alias_binding_name(alias, env)?;
    context
        .sink_bindings
        .get(&binding_name)
        .copied()
        .ok_or_else(|| format!("generic view lowering does not know sink alias `{binding_name}`"))
}

fn extract_placeholder_text<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<String, String> {
    let Some(placeholder) = find_named_argument(arguments, "placeholder") else {
        return Ok(String::new());
    };
    let text = find_object_field_expression(placeholder, "text")?
        .ok_or_else(|| "placeholder requires `text`".to_string())?;
    resolve_static_text_expression(text, context, env)
}

fn extract_static_bool_literal(expression: &StaticSpannedExpression) -> Result<bool, String> {
    match &expression.node {
        StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Text(tag)) => {
            match tag.as_str() {
                "True" => Ok(true),
                "False" => Ok(false),
                _ => Err("generic view lowering requires True/False literal".to_string()),
            }
        }
        _ => Err("generic view lowering requires True/False literal".to_string()),
    }
}

fn extract_optional_disabled_sink<'a>(
    style: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Result<Option<SinkPortId>, String> {
    let Some(disabled) = find_object_field_expression(style, "disabled")? else {
        return Ok(None);
    };
    match &disabled.node {
        StaticExpression::Alias(_) => {
            resolve_view_sink_expression(disabled, context, env).map(Some)
        }
        _ => Ok(origin_binding.and_then(|binding| {
            context
                .sink_bindings
                .get(&format!("{binding}.disabled"))
                .copied()
        })),
    }
}

fn resolve_select_selected_sink<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
    origin_binding: Option<&str>,
) -> Result<SinkPortId, String> {
    let selected = find_named_argument(arguments, "selected")
        .ok_or_else(|| "Element/select requires `selected`".to_string())?;
    match &selected.node {
        StaticExpression::Alias(_) => resolve_view_sink_expression(selected, context, env),
        _ => origin_binding
            .and_then(|binding| {
                context
                    .sink_bindings
                    .get(&format!("{binding}.selected"))
                    .copied()
            })
            .ok_or_else(|| {
                "generic view lowering requires sink-backed `selected` alias or linked selected sink"
                    .to_string()
            }),
    }
}

fn extract_select_options<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<Vec<HostSelectOption>, String> {
    let options_expression = find_named_argument(arguments, "options")
        .ok_or_else(|| "Element/select requires `options`".to_string())?;
    let StaticExpression::List { items } = &options_expression.node else {
        return Err("Element/select requires `options: LIST { ... }`".to_string());
    };

    items
        .iter()
        .map(|item| {
            let value = find_object_field_expression(item, "value")?
                .ok_or_else(|| "select option requires `value`".to_string())?;
            let label = find_object_field_expression(item, "label")?
                .ok_or_else(|| "select option requires `label`".to_string())?;
            Ok(HostSelectOption {
                value: resolve_static_text_expression(value, context, env)?,
                label: resolve_static_text_expression(label, context, env)?,
            })
        })
        .collect()
}

fn extract_static_number_string<'a>(
    expression: &'a StaticSpannedExpression,
    context: &GenericViewLoweringContext<'a>,
    env: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<String, String> {
    resolve_static_number_expression(expression, context, env).map(|number| number.to_string())
}

fn alias_path_name(path: &[boon::parser::StrSlice]) -> String {
    path.iter()
        .map(|segment| segment.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

fn alias_bounces_to_same_top_level_name(
    expression: &StaticSpannedExpression,
    expected_name: &str,
) -> bool {
    matches!(
        &expression.node,
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) if parts.len() == 1 && parts[0].as_str() == expected_name
    )
}

fn next_free_view_site(host_view: &HostViewIr) -> u32 {
    fn walk(node: &HostViewNode) -> u32 {
        node.children
            .iter()
            .fold(node.retained_key.view_site.0, |max_site, child| {
                max_site.max(walk(child))
            })
    }

    host_view
        .root
        .as_ref()
        .map_or(1, |root| walk(root).saturating_add(1))
}

fn append_timer_source_child(
    host_view: &mut HostViewIr,
    function_instance: FunctionInstanceId,
    tick_port: SourcePortId,
    interval_ms: u32,
    view_site_counter: &mut u32,
) {
    let view_site = ViewSiteId(*view_site_counter);
    *view_site_counter += 1;
    let timer_node = HostViewNode {
        retained_key: RetainedNodeKey {
            view_site,
            function_instance: Some(function_instance),
            mapped_item_identity: None,
        },
        kind: HostViewKind::TimerSource {
            tick_port,
            interval_ms,
        },
        children: Vec::new(),
    };

    if let Some(root) = host_view.root.as_mut() {
        if let Some(content_root) = root.children.first_mut() {
            content_root.children.push(timer_node);
        } else {
            root.children.push(timer_node);
        }
    }
}

fn lower_timer_backed_signal_host_view<'a>(
    expressions: &'a [StaticSpannedExpression],
    sink_bindings: &[(&str, SinkPortId)],
    press_bindings: &[(&str, SourcePortId)],
    root_view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    root_binding_name: Option<&str>,
    tick_port: SourcePortId,
    interval_ms: u32,
) -> Result<HostViewIr, String> {
    let mut host_view = lower_generic_host_view_with_root_binding(
        expressions,
        sink_bindings,
        press_bindings,
        root_view_site,
        function_instance,
        root_binding_name,
    )?;
    let mut view_site_counter = next_free_view_site(&host_view);
    append_timer_source_child(
        &mut host_view,
        function_instance,
        tick_port,
        interval_ms,
        &mut view_site_counter,
    );
    Ok(host_view)
}

struct TimerBackedSignalProgramConfig<'a> {
    ir: IrProgram,
    sink_bindings: &'a [(&'a str, SinkPortId)],
    press_bindings: &'a [(&'a str, SourcePortId)],
    root_view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    root_binding_name: Option<&'a str>,
    value_sink: SinkPortId,
    tick_port: SourcePortId,
    interval_ms: u32,
}

#[derive(Clone, Copy)]
struct TimerBackedSignalSurfaceConfig<'a> {
    sink_bindings: &'a [(&'a str, SinkPortId)],
    press_bindings: &'a [(&'a str, SourcePortId)],
    root_view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    root_binding_name: Option<&'a str>,
    value_sink: SinkPortId,
    tick_port: SourcePortId,
}

fn lower_timer_backed_signal_program(
    expressions: &[StaticSpannedExpression],
    config: &TimerBackedSignalProgramConfig<'_>,
) -> Result<IntervalProgram, String> {
    Ok(IntervalProgram {
        ir: config.ir.clone(),
        host_view: lower_timer_backed_signal_host_view(
            expressions,
            config.sink_bindings,
            config.press_bindings,
            config.root_view_site,
            config.function_instance,
            config.root_binding_name,
            config.tick_port,
            config.interval_ms,
        )?,
        value_sink: config.value_sink,
        tick_port: config.tick_port,
        interval_ms: config.interval_ms,
    })
}

fn lower_timer_backed_signal_surface<'a, F>(
    expressions: &'a [StaticSpannedExpression],
    config: &TimerBackedSignalSurfaceConfig<'_>,
    build_ir: F,
) -> Result<IntervalProgram, String>
where
    F: FnOnce(
        &BTreeMap<String, &'a StaticSpannedExpression>,
        SourcePortId,
        SinkPortId,
    ) -> Result<(u32, IrProgram), String>,
{
    lower_with_bindings(expressions, |bindings| {
        let (interval_ms, ir) = build_ir(&bindings, config.tick_port, config.value_sink)?;
        lower_timer_backed_signal_program(
            expressions,
            &TimerBackedSignalProgramConfig {
                ir,
                sink_bindings: config.sink_bindings,
                press_bindings: config.press_bindings,
                root_view_site: config.root_view_site,
                function_instance: config.function_instance,
                root_binding_name: config.root_binding_name,
                value_sink: config.value_sink,
                tick_port: config.tick_port,
                interval_ms,
            },
        )
    })
}

define_source_parsed_entrypoint!(
    try_lower_counter,
    try_lower_counter_from_expressions,
    CounterProgram
);

fn try_lower_counter_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<CounterProgram, String> {
    lower_bindings_group_typed_program(
        expressions,
        &COUNTER_BINDINGS_GENERIC_HOST_IR_GROUP,
        "single_action_accumulator_document",
        |program| match program {
            LoweredProgram::Counter(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_todo_mvc,
    try_lower_todo_mvc_from_expressions,
    TodoProgram
);

fn try_lower_todo_mvc_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<TodoProgram, String> {
    lower_bindings_group_typed_program(
        expressions,
        &LIST_PERSISTENT_SEMANTIC_BINDINGS_GROUP,
        "editable_filterable_list_document",
        |program| match program {
            LoweredProgram::TodoMvc(program)
            | LoweredProgram::TodoMvcWithInitialTodos { program, .. } => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_todo_mvc_physical,
    try_lower_todo_mvc_physical_from_expressions,
    TodoPhysicalProgram
);

fn try_lower_todo_mvc_physical_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<TodoPhysicalProgram, String> {
    lower_validation_only_typed_program(
        expressions,
        const {
            &ValidationOnlyTypedConfig {
                validation: StructuralValidationSpec {
                    subset: "routed_task_scene_document",
                    top_level_bindings: &["store", "scene"],
                    required_paths: &[],
                    hold_paths: &[],
                    required_functions: &["new_todo", "theme_switcher"],
                    alias_paths: &[],
                    function_call_paths: &[
                        ["Router", "go_to"].as_slice(),
                        ["Router", "route"].as_slice(),
                        ["Scene", "new"].as_slice(),
                        ["Scene", "Element", "text_input"].as_slice(),
                        ["Scene", "Element", "checkbox"].as_slice(),
                        ["Scene", "Element", "button"].as_slice(),
                    ],
                    text_fragments: &[
                        "Dark mode",
                        "Professional",
                        "Glassmorphism",
                        "Neobrutalism",
                        "Neumorphism",
                        "Created by",
                        "TodoMVC",
                    ],
                    require_hold: false,
                    require_latest: false,
                    require_then: false,
                    require_when: false,
                    require_while: false,
                },
                build_output: || TodoPhysicalProgram,
            }
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_complex_counter,
    try_lower_complex_counter_from_expressions,
    ComplexCounterProgram
);

fn try_lower_complex_counter_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ComplexCounterProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_IR_SURFACE_GROUP,
        "dual_action_accumulator_document",
        |program| match program {
            LoweredProgram::ComplexCounter(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_list_retain_reactive,
    try_lower_list_retain_reactive_from_expressions,
    ListRetainReactiveProgram
);

fn try_lower_list_retain_reactive_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ListRetainReactiveProgram, String> {
    lower_bindings_group_typed_program(
        expressions,
        &LIST_BOOL_TOGGLE_SEMANTIC_BINDINGS_GROUP,
        "retained_toggle_filter_list_document",
        |program| match program {
            LoweredProgram::ListRetainReactive(program) => Some(program),
            _ => None,
        },
    )
}

struct BoolToggleRuntimeConfig<'a> {
    base_node_id: u32,
    toggle_press_port: SourcePortId,
    mode_sink: SinkPortId,
    initial_value: bool,
    true_label: &'a str,
    false_label: &'a str,
}

struct BoolToggleRuntime {
    nodes: Vec<IrNode>,
    next_node_id: u32,
    hold: NodeId,
    persistence: Vec<IrNodePersistence>,
}

#[derive(Clone, Copy)]
enum BoolToggleListAuxConfig<'a> {
    StaticText { sink: SinkPortId, text: &'a str },
    CountText { sink: SinkPortId, prefix: &'a str },
}

struct BoolToggleListProgramConfig<'a> {
    runtime: BoolToggleRuntimeConfig<'a>,
    items_list_sink: SinkPortId,
    aux: BoolToggleListAuxConfig<'a>,
}

struct BoolToggleListValues {
    false_values: Vec<KernelValue>,
    true_values: Vec<KernelValue>,
}

#[derive(Clone, Copy)]
struct StaticBoolToggleListValuesConfig<'a> {
    initial_value: bool,
    false_values: &'a [&'a str],
    true_values: &'a [&'a str],
}

#[derive(Clone, Copy)]
struct HoldBackedIntegerSubsetBoolToggleListValuesConfig<'a> {
    toggle_path: &'a [&'a str],
    source_list_path: &'a [&'a str],
    selected_subset_path: &'a [&'a str],
    item_alias: &'a str,
}

struct BoolToggleListSemanticLowering {
    initial_value: bool,
    false_values: Vec<KernelValue>,
    true_values: Vec<KernelValue>,
}

#[derive(Clone, Copy)]
enum BoolToggleListValueDerivation<'a> {
    StaticValues(StaticBoolToggleListValuesConfig<'a>),
    HoldBackedIntegerSubset(HoldBackedIntegerSubsetBoolToggleListValuesConfig<'a>),
}

struct BoolToggleListSemanticConfig<'a> {
    subset: &'static str,
    program: BoolToggleListProgramConfig<'a>,
    persistence: &'a [LoweringPathPersistenceConfig<'a>],
    derivation: BoolToggleListValueDerivation<'a>,
}

#[derive(Clone, Copy)]
pub(crate) struct LoweringPathPersistenceConfig<'a> {
    pub(crate) path: &'a [&'a str],
    pub(crate) node: NodeId,
    pub(crate) local_slot: u32,
    pub(crate) persist_kind: PersistKind,
}

#[derive(Clone, Copy)]
struct LoweringPathPersistenceSeed<'a> {
    path: &'a [&'a str],
    local_slot: u32,
    persist_kind: PersistKind,
}

fn lower_bool_toggle_runtime(
    persistence: Vec<IrNodePersistence>,
    config: &BoolToggleRuntimeConfig<'_>,
) -> BoolToggleRuntime {
    let base = config.base_node_id;
    let hold = NodeId(base + 4);
    let mut nodes = Vec::new();
    append_literal(&mut nodes, KernelValue::from(config.initial_value), base);
    let toggle_press = append_source_port(&mut nodes, config.toggle_press_port, base + 1);
    append_bool_not(&mut nodes, hold, base + 2);
    append_then(&mut nodes, toggle_press, NodeId(base + 2), base + 3);
    nodes.push(IrNode {
        id: hold,
        source_expr: None,
        kind: IrNodeKind::Hold {
            seed: NodeId(base),
            updates: NodeId(base + 3),
        },
    });
    append_literal(&mut nodes, KernelValue::from(config.true_label), base + 5);
    append_literal(&mut nodes, KernelValue::from(config.false_label), base + 6);
    append_when(
        &mut nodes,
        hold,
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(base + 5),
        }],
        NodeId(base + 6),
        base + 7,
    );
    append_value_sink(&mut nodes, NodeId(base + 7), config.mode_sink, base + 8);

    BoolToggleRuntime {
        nodes,
        next_node_id: base + 9,
        hold,
        persistence,
    }
}

fn append_kernel_list_literal<I>(
    nodes: &mut Vec<IrNode>,
    next_node_id: &mut u32,
    values: I,
) -> NodeId
where
    I: IntoIterator<Item = KernelValue>,
{
    let item_ids = values
        .into_iter()
        .map(|value| {
            let node = append_literal(nodes, value, *next_node_id);
            *next_node_id += 1;
            node
        })
        .collect::<Vec<_>>();
    let list = NodeId(*next_node_id);
    *next_node_id += 1;
    nodes.push(IrNode {
        id: list,
        source_expr: None,
        kind: IrNodeKind::ListLiteral { items: item_ids },
    });
    list
}

#[derive(Clone, Copy)]
struct SourceBackedHoldNodes {
    source: NodeId,
    hold: NodeId,
}

struct SourceBackedHoldConfig {
    seed: NodeId,
    source_port: SourcePortId,
    source_node_id: u32,
    hold_node_id: u32,
}

#[derive(Clone, Copy)]
struct TriggeredUpdateConfig {
    source: NodeId,
    body: NodeId,
    then_node_id: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LatestHoldMode {
    AlwaysCreateLatest,
    OnlyWhenMultiple,
}

fn append_seeded_hold(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    updates: NodeId,
    hold_node_id: u32,
) -> NodeId {
    let hold = NodeId(hold_node_id);
    nodes.push(IrNode {
        id: hold,
        source_expr: None,
        kind: IrNodeKind::Hold { seed, updates },
    });
    hold
}

fn append_seeded_hold_sink(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    updates: NodeId,
    hold_node_id: u32,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let hold = append_seeded_hold(nodes, seed, updates, hold_node_id);
    append_value_sink(nodes, hold, sink_port, sink_node_id);
    hold
}

fn append_value_sink(
    nodes: &mut Vec<IrNode>,
    input: NodeId,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    nodes.push(IrNode {
        id: NodeId(sink_node_id),
        source_expr: None,
        kind: IrNodeKind::SinkPort {
            port: sink_port,
            input,
        },
    });
    input
}

fn append_literal_sink(
    nodes: &mut Vec<IrNode>,
    value: KernelValue,
    literal_node_id: u32,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let literal = append_literal(nodes, value, literal_node_id);
    append_value_sink(nodes, literal, sink_port, sink_node_id)
}

fn append_list_count_sink(
    nodes: &mut Vec<IrNode>,
    list: NodeId,
    count_node_id: u32,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let count = append_list_count(nodes, list, count_node_id);
    append_value_sink(nodes, count, sink_port, sink_node_id)
}

fn append_list_all_object_bool_field_sink(
    nodes: &mut Vec<IrNode>,
    list: NodeId,
    field: &str,
    all_node_id: u32,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let all = append_list_all_object_bool_field(nodes, list, field, all_node_id);
    append_value_sink(nodes, all, sink_port, sink_node_id)
}

fn append_prefixed_list_count_sink(
    nodes: &mut Vec<IrNode>,
    list: NodeId,
    prefix: &str,
    count_node_id: u32,
    prefix_node_id: u32,
    text_join_node_id: u32,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    append_decorated_list_count_sink(
        nodes,
        list,
        count_node_id,
        Some((prefix, prefix_node_id)),
        None,
        text_join_node_id,
        sink_port,
        sink_node_id,
    )
}

fn append_decorated_list_count_sink(
    nodes: &mut Vec<IrNode>,
    list: NodeId,
    count_node_id: u32,
    prefix: Option<(&str, u32)>,
    suffix: Option<(&str, u32)>,
    text_join_node_id: u32,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let count = append_list_count(nodes, list, count_node_id);
    let mut inputs =
        Vec::with_capacity(1 + usize::from(prefix.is_some()) + usize::from(suffix.is_some()));
    if let Some((prefix_text, prefix_node_id)) = prefix {
        let prefix_node = append_literal(nodes, KernelValue::from(prefix_text), prefix_node_id);
        inputs.push(prefix_node);
    }
    inputs.push(count);
    if let Some((suffix_text, suffix_node_id)) = suffix {
        let suffix_node = append_literal(nodes, KernelValue::from(suffix_text), suffix_node_id);
        inputs.push(suffix_node);
    }
    append_text_join_sink(nodes, inputs, text_join_node_id, sink_port, sink_node_id)
}

fn append_source_backed_hold(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    source_port: SourcePortId,
    source_node_id: u32,
    hold_node_id: u32,
) -> SourceBackedHoldNodes {
    let source = append_source_port(nodes, source_port, source_node_id);
    let hold = NodeId(hold_node_id);
    append_seeded_hold(nodes, seed, source, hold.0);
    SourceBackedHoldNodes { source, hold }
}

fn append_source_backed_holds(
    nodes: &mut Vec<IrNode>,
    configs: &[SourceBackedHoldConfig],
) -> Vec<SourceBackedHoldNodes> {
    configs
        .iter()
        .map(|config| {
            append_source_backed_hold(
                nodes,
                config.seed,
                config.source_port,
                config.source_node_id,
                config.hold_node_id,
            )
        })
        .collect()
}

fn append_source_port(
    nodes: &mut Vec<IrNode>,
    source_port: SourcePortId,
    source_node_id: u32,
) -> NodeId {
    let source = NodeId(source_node_id);
    nodes.push(IrNode {
        id: source,
        source_expr: None,
        kind: IrNodeKind::SourcePort(source_port),
    });
    source
}

fn append_field_read(
    nodes: &mut Vec<IrNode>,
    object: NodeId,
    field: &str,
    field_node_id: u32,
) -> NodeId {
    let read = NodeId(field_node_id);
    nodes.push(IrNode {
        id: read,
        source_expr: None,
        kind: IrNodeKind::FieldRead {
            object,
            field: field.to_string(),
        },
    });
    read
}

fn append_literal(nodes: &mut Vec<IrNode>, value: KernelValue, literal_node_id: u32) -> NodeId {
    let literal = NodeId(literal_node_id);
    nodes.push(IrNode {
        id: literal,
        source_expr: None,
        kind: IrNodeKind::Literal(value),
    });
    literal
}

fn append_mirror_cell(nodes: &mut Vec<IrNode>, cell: MirrorCellId, mirror_node_id: u32) -> NodeId {
    let mirror = NodeId(mirror_node_id);
    nodes.push(IrNode {
        id: mirror,
        source_expr: None,
        kind: IrNodeKind::MirrorCell(cell),
    });
    mirror
}

fn append_bool_not(nodes: &mut Vec<IrNode>, input: NodeId, not_node_id: u32) -> NodeId {
    let not = NodeId(not_node_id);
    nodes.push(IrNode {
        id: not,
        source_expr: None,
        kind: IrNodeKind::BoolNot { input },
    });
    not
}

fn append_then(nodes: &mut Vec<IrNode>, source: NodeId, body: NodeId, then_node_id: u32) -> NodeId {
    let then = NodeId(then_node_id);
    nodes.push(IrNode {
        id: then,
        source_expr: None,
        kind: IrNodeKind::Then { source, body },
    });
    then
}

fn append_when(
    nodes: &mut Vec<IrNode>,
    source: NodeId,
    arms: Vec<crate::ir::MatchArm>,
    fallback: NodeId,
    when_node_id: u32,
) -> NodeId {
    let when = NodeId(when_node_id);
    nodes.push(IrNode {
        id: when,
        source_expr: None,
        kind: IrNodeKind::When {
            source,
            arms,
            fallback,
        },
    });
    when
}

fn append_while(
    nodes: &mut Vec<IrNode>,
    source: NodeId,
    arms: Vec<crate::ir::MatchArm>,
    fallback: NodeId,
    while_node_id: u32,
) -> NodeId {
    let choice = NodeId(while_node_id);
    nodes.push(IrNode {
        id: choice,
        source_expr: None,
        kind: IrNodeKind::While {
            source,
            arms,
            fallback,
        },
    });
    choice
}

fn append_list_count(nodes: &mut Vec<IrNode>, list: NodeId, count_node_id: u32) -> NodeId {
    let count = NodeId(count_node_id);
    nodes.push(IrNode {
        id: count,
        source_expr: None,
        kind: IrNodeKind::ListCount { list },
    });
    count
}

fn append_list_all_object_bool_field(
    nodes: &mut Vec<IrNode>,
    list: NodeId,
    field: &str,
    all_node_id: u32,
) -> NodeId {
    let all = NodeId(all_node_id);
    nodes.push(IrNode {
        id: all,
        source_expr: None,
        kind: IrNodeKind::ListAllObjectBoolField {
            list,
            field: field.to_string(),
        },
    });
    all
}

fn append_key_down_match(
    nodes: &mut Vec<IrNode>,
    input: NodeId,
    matcher: &str,
    result: NodeId,
    fallback: NodeId,
    key_node_id: u32,
    match_node_id: u32,
) -> NodeId {
    let key = NodeId(key_node_id);
    nodes.push(IrNode {
        id: key,
        source_expr: None,
        kind: IrNodeKind::KeyDownKey { input },
    });
    append_when(
        nodes,
        key,
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(matcher),
            result,
        }],
        fallback,
        match_node_id,
    )
}

fn append_trimmed_text(nodes: &mut Vec<IrNode>, input: NodeId, trimmed_node_id: u32) -> NodeId {
    let trimmed = NodeId(trimmed_node_id);
    nodes.push(IrNode {
        id: trimmed,
        source_expr: None,
        kind: IrNodeKind::TextTrim { input },
    });
    trimmed
}

fn append_non_empty_value_or_skip(
    nodes: &mut Vec<IrNode>,
    value: NodeId,
    empty_value: NodeId,
    non_empty_value: NodeId,
    empty_check_node_id: u32,
    gated_value_node_id: u32,
    skip: NodeId,
) -> NodeId {
    let empty_check = NodeId(empty_check_node_id);
    nodes.push(IrNode {
        id: empty_check,
        source_expr: None,
        kind: IrNodeKind::Eq {
            lhs: value,
            rhs: empty_value,
        },
    });
    append_when(
        nodes,
        empty_check,
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: skip,
        }],
        non_empty_value,
        gated_value_node_id,
    )
}

fn append_latest_inputs(
    nodes: &mut Vec<IrNode>,
    inputs: Vec<NodeId>,
    latest_node_id: u32,
    mode: LatestHoldMode,
) -> NodeId {
    if mode == LatestHoldMode::OnlyWhenMultiple && inputs.len() == 1 {
        inputs[0]
    } else {
        let latest = NodeId(latest_node_id);
        nodes.push(IrNode {
            id: latest,
            source_expr: None,
            kind: IrNodeKind::Latest { inputs },
        });
        latest
    }
}

fn append_triggered_updates(
    nodes: &mut Vec<IrNode>,
    updates: &[TriggeredUpdateConfig],
) -> Vec<NodeId> {
    updates
        .iter()
        .map(|update| append_then(nodes, update.source, update.body, update.then_node_id))
        .collect()
}

fn append_latest_inputs_with_triggered_updates(
    nodes: &mut Vec<IrNode>,
    mut base_updates: Vec<NodeId>,
    triggered_updates: &[TriggeredUpdateConfig],
    latest_node_id: u32,
    mode: LatestHoldMode,
) -> NodeId {
    base_updates.extend(append_triggered_updates(nodes, triggered_updates));
    append_latest_inputs(nodes, base_updates, latest_node_id, mode)
}

fn append_latest_hold(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    update_inputs: Vec<NodeId>,
    latest_node_id: u32,
    hold_node_id: u32,
    mode: LatestHoldMode,
) -> NodeId {
    let updates = append_latest_inputs(nodes, update_inputs, latest_node_id, mode);

    append_seeded_hold(nodes, seed, updates, hold_node_id)
}

fn append_latest_hold_sink_with_triggered_updates(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    base_updates: Vec<NodeId>,
    triggered_updates: &[TriggeredUpdateConfig],
    latest_node_id: u32,
    hold_node_id: u32,
    mode: LatestHoldMode,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let updates = append_latest_inputs_with_triggered_updates(
        nodes,
        base_updates,
        triggered_updates,
        latest_node_id,
        mode,
    );
    append_seeded_hold_sink(nodes, seed, updates, hold_node_id, sink_port, sink_node_id)
}

fn append_text_join(
    nodes: &mut Vec<IrNode>,
    inputs: Vec<NodeId>,
    text_join_node_id: u32,
) -> NodeId {
    let joined = NodeId(text_join_node_id);
    nodes.push(IrNode {
        id: joined,
        source_expr: None,
        kind: IrNodeKind::TextJoin { inputs },
    });
    joined
}

fn append_text_join_sink(
    nodes: &mut Vec<IrNode>,
    inputs: Vec<NodeId>,
    text_join_node_id: u32,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let joined = append_text_join(nodes, inputs, text_join_node_id);
    append_value_sink(nodes, joined, sink_port, sink_node_id);
    joined
}

fn append_list_state_hold(
    nodes: &mut Vec<IrNode>,
    current_list: NodeId,
    seed_list: NodeId,
    appended_item: NodeId,
    append_node_id: u32,
    additional_update_inputs: Vec<NodeId>,
    latest_node_id: u32,
    hold_node_id: u32,
) -> NodeId {
    let append = NodeId(append_node_id);
    nodes.push(IrNode {
        id: append,
        source_expr: None,
        kind: IrNodeKind::ListAppend {
            list: current_list,
            item: appended_item,
        },
    });

    let mut updates = Vec::with_capacity(1 + additional_update_inputs.len());
    updates.push(append);
    updates.extend(additional_update_inputs);

    append_latest_hold(
        nodes,
        seed_list,
        updates,
        latest_node_id,
        hold_node_id,
        LatestHoldMode::AlwaysCreateLatest,
    )
}

#[derive(Clone, Copy)]
struct SourceTriggeredBodyConfig {
    source: NodeId,
    body: NodeId,
    then: NodeId,
}

#[derive(Clone, Copy)]
struct SourcePortTriggeredBodyConfig {
    source_port: SourcePortId,
    body: NodeId,
    source_node_id: u32,
    then_node_id: u32,
}

#[derive(Clone, Copy)]
struct SourceDeltaAccumulatorConfig {
    source_port: SourcePortId,
    delta: f64,
    source_node_id: u32,
    delta_node_id: u32,
    sum_node_id: u32,
    then_node_id: u32,
}

#[derive(Clone)]
struct SourceTriggeredLiteralConfig {
    source_port: SourcePortId,
    literal: KernelValue,
    source_node_id: u32,
    literal_node_id: u32,
    then_node_id: u32,
}

fn append_source_triggered_literal_updates(
    nodes: &mut Vec<IrNode>,
    updates: &[SourceTriggeredLiteralConfig],
    latest_node_id: u32,
    mode: LatestHoldMode,
) -> NodeId {
    let mut lowered = Vec::with_capacity(updates.len());
    for update in updates {
        let source = append_source_port(nodes, update.source_port, update.source_node_id);
        let body = append_literal(nodes, update.literal.clone(), update.literal_node_id);
        lowered.push(SourceTriggeredBodyConfig {
            source,
            body,
            then: NodeId(update.then_node_id),
        });
    }

    append_source_triggered_updates(nodes, &lowered, latest_node_id, mode)
}

fn append_source_triggered_literal_hold_sink(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    updates: &[SourceTriggeredLiteralConfig],
    latest_node_id: u32,
    hold_node_id: u32,
    mode: LatestHoldMode,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let updates = append_source_triggered_literal_updates(nodes, updates, latest_node_id, mode);
    append_seeded_hold_sink(nodes, seed, updates, hold_node_id, sink_port, sink_node_id)
}

fn append_source_triggered_updates(
    nodes: &mut Vec<IrNode>,
    updates: &[SourceTriggeredBodyConfig],
    latest_node_id: u32,
    mode: LatestHoldMode,
) -> NodeId {
    let mut then_nodes = Vec::with_capacity(updates.len());
    for update in updates {
        then_nodes.push(append_then(
            nodes,
            update.source,
            update.body,
            update.then.0,
        ));
    }

    append_latest_inputs(nodes, then_nodes, latest_node_id, mode)
}

fn append_source_port_triggered_updates(
    nodes: &mut Vec<IrNode>,
    updates: &[SourcePortTriggeredBodyConfig],
    latest_node_id: u32,
    mode: LatestHoldMode,
) -> NodeId {
    let lowered = updates
        .iter()
        .map(|update| SourceTriggeredBodyConfig {
            source: append_source_port(nodes, update.source_port, update.source_node_id),
            body: update.body,
            then: NodeId(update.then_node_id),
        })
        .collect::<Vec<_>>();
    append_source_triggered_updates(nodes, &lowered, latest_node_id, mode)
}

fn append_source_triggered_hold(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    updates: &[SourceTriggeredBodyConfig],
    latest_node_id: u32,
    hold_node_id: u32,
    update_merge_mode: LatestHoldMode,
) -> NodeId {
    let updates =
        append_source_triggered_updates(nodes, updates, latest_node_id, update_merge_mode);
    append_latest_hold(
        nodes,
        seed,
        vec![updates],
        latest_node_id,
        hold_node_id,
        LatestHoldMode::OnlyWhenMultiple,
    )
}

fn append_source_port_triggered_hold(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    updates: &[SourcePortTriggeredBodyConfig],
    latest_node_id: u32,
    hold_node_id: u32,
    update_merge_mode: LatestHoldMode,
) -> NodeId {
    let updates =
        append_source_port_triggered_updates(nodes, updates, latest_node_id, update_merge_mode);
    append_latest_hold(
        nodes,
        seed,
        vec![updates],
        latest_node_id,
        hold_node_id,
        LatestHoldMode::OnlyWhenMultiple,
    )
}

fn append_source_delta_accumulator_hold(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    hold_node_id: u32,
    actions: &[SourceDeltaAccumulatorConfig],
    latest_node_id: u32,
    merge_mode: LatestHoldMode,
) -> NodeId {
    let hold = NodeId(hold_node_id);
    let mut updates = Vec::with_capacity(actions.len());
    for action in actions {
        let source = append_source_port(nodes, action.source_port, action.source_node_id);
        let delta = append_literal(nodes, KernelValue::from(action.delta), action.delta_node_id);
        let sum = NodeId(action.sum_node_id);
        let then = NodeId(action.then_node_id);
        nodes.push(IrNode {
            id: sum,
            source_expr: None,
            kind: IrNodeKind::Add {
                lhs: hold,
                rhs: delta,
            },
        });
        updates.push(SourceTriggeredBodyConfig {
            source,
            body: sum,
            then,
        });
    }

    append_source_triggered_hold(
        nodes,
        seed,
        &updates,
        latest_node_id,
        hold_node_id,
        merge_mode,
    )
}

fn append_source_delta_accumulator_hold_sink(
    nodes: &mut Vec<IrNode>,
    seed: NodeId,
    hold_node_id: u32,
    actions: &[SourceDeltaAccumulatorConfig],
    latest_node_id: u32,
    merge_mode: LatestHoldMode,
    sink_port: SinkPortId,
    sink_node_id: u32,
) -> NodeId {
    let hold = append_source_delta_accumulator_hold(
        nodes,
        seed,
        hold_node_id,
        actions,
        latest_node_id,
        merge_mode,
    );
    append_value_sink(nodes, hold, sink_port, sink_node_id);
    hold
}

fn lower_bool_toggle_list_ir<F, T>(
    persistence: Vec<IrNodePersistence>,
    toggle_config: &BoolToggleRuntimeConfig<'_>,
    false_values: F,
    true_values: T,
    items_list_sink: SinkPortId,
    aux: BoolToggleListAuxConfig<'_>,
) -> IrProgram
where
    F: IntoIterator<Item = KernelValue>,
    T: IntoIterator<Item = KernelValue>,
{
    let toggle_runtime = lower_bool_toggle_runtime(persistence, toggle_config);
    let hold_node = toggle_runtime.hold;
    let mut nodes = toggle_runtime.nodes;
    let mut next_node_id = toggle_runtime.next_node_id;
    let false_list = append_kernel_list_literal(&mut nodes, &mut next_node_id, false_values);
    let true_list = append_kernel_list_literal(&mut nodes, &mut next_node_id, true_values);
    let selected_list = append_when(
        &mut nodes,
        hold_node,
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: true_list,
        }],
        false_list,
        next_node_id,
    );
    next_node_id += 1;

    match aux {
        BoolToggleListAuxConfig::StaticText { sink, text } => {
            let text_node = append_literal(&mut nodes, KernelValue::from(text), next_node_id);
            next_node_id += 1;
            append_value_sink(&mut nodes, text_node, sink, next_node_id);
            next_node_id += 1;
        }
        BoolToggleListAuxConfig::CountText { sink, prefix } => {
            append_prefixed_list_count_sink(
                &mut nodes,
                selected_list,
                prefix,
                next_node_id,
                next_node_id + 1,
                next_node_id + 2,
                sink,
                next_node_id + 3,
            );
            next_node_id += 4;
        }
    }

    append_value_sink(&mut nodes, selected_list, items_list_sink, next_node_id);

    IrProgram {
        nodes,
        functions: Vec::new(),
        persistence: toggle_runtime.persistence,
    }
}

fn lower_bool_toggle_list_program(
    persistence: Vec<IrNodePersistence>,
    config: &BoolToggleListProgramConfig<'_>,
    values: BoolToggleListValues,
) -> IrProgram {
    lower_bool_toggle_list_ir(
        persistence,
        &config.runtime,
        values.false_values,
        values.true_values,
        config.items_list_sink,
        config.aux,
    )
}

fn derive_static_bool_toggle_list_semantics(
    config: StaticBoolToggleListValuesConfig<'_>,
) -> Result<BoolToggleListSemanticLowering, String> {
    let StaticBoolToggleListValuesConfig {
        initial_value,
        false_values,
        true_values,
    } = config;

    Ok(BoolToggleListSemanticLowering {
        initial_value,
        false_values: false_values
            .iter()
            .copied()
            .map(KernelValue::from)
            .collect(),
        true_values: true_values.iter().copied().map(KernelValue::from).collect(),
    })
}

fn derive_hold_backed_integer_subset_bool_toggle_list_semantics(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    subset: &str,
    config: HoldBackedIntegerSubsetBoolToggleListValuesConfig<'_>,
) -> Result<BoolToggleListSemanticLowering, String> {
    let HoldBackedIntegerSubsetBoolToggleListValuesConfig {
        toggle_path,
        source_list_path,
        selected_subset_path,
        item_alias,
    } = config;

    let show_toggle_binding = binding_at_path(bindings, toggle_path)
        .ok_or_else(|| format!("{subset} subset requires `{}`", toggle_path.join(".")))?;
    let source_values_binding = binding_at_path(bindings, source_list_path)
        .ok_or_else(|| format!("{subset} subset requires `{}`", source_list_path.join(".")))?;
    let selected_subset_binding =
        binding_at_path(bindings, selected_subset_path).ok_or_else(|| {
            format!(
                "{subset} subset requires `{}`",
                selected_subset_path.join(".")
            )
        })?;

    let initial_value = extract_hold_seed_bool(show_toggle_binding, subset)?;
    let source_values = extract_integer_list(source_values_binding, subset)?;
    let selected_values =
        extract_selected_integer_subset_values(selected_subset_binding, subset, item_alias)?;
    if selected_values
        .iter()
        .any(|value| !source_values.contains(value))
    {
        return Err(format!(
            "{subset} subset requires selected values to come from `{}`",
            source_list_path.join(".")
        ));
    }

    Ok(BoolToggleListSemanticLowering {
        initial_value,
        false_values: source_values
            .into_iter()
            .map(|value| KernelValue::from(value as f64))
            .collect(),
        true_values: selected_values
            .into_iter()
            .map(|value| KernelValue::from(value as f64))
            .collect(),
    })
}

fn lower_bool_toggle_list_semantic_ir(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &BoolToggleListSemanticConfig<'_>,
) -> Result<IrProgram, String> {
    let lowering = match config.derivation {
        BoolToggleListValueDerivation::StaticValues(values) => {
            derive_static_bool_toggle_list_semantics(values)?
        }
        BoolToggleListValueDerivation::HoldBackedIntegerSubset(subset) => {
            derive_hold_backed_integer_subset_bool_toggle_list_semantics(
                bindings,
                config.subset,
                subset,
            )?
        }
    };

    let runtime = BoolToggleRuntimeConfig {
        initial_value: lowering.initial_value,
        ..config.program.runtime
    };
    let program = BoolToggleListProgramConfig {
        runtime,
        ..config.program
    };
    Ok(lower_bool_toggle_list_program(
        collect_path_lowering_persistence_from_configs(bindings, config.persistence),
        &program,
        BoolToggleListValues {
            false_values: lowering.false_values,
            true_values: lowering.true_values,
        },
    ))
}

fn build_empty_persistent_ir_program(persistence: Vec<IrNodePersistence>) -> IrProgram {
    IrProgram {
        nodes: Vec::new(),
        functions: Vec::new(),
        persistence,
    }
}

fn extract_hold_seed_bool(
    expression: &StaticSpannedExpression,
    subset: &str,
) -> Result<bool, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(format!("{subset} subset requires piped HOLD input"));
    };
    let StaticExpression::Hold { .. } = &to.node else {
        return Err(format!("{subset} subset requires HOLD input"));
    };
    extract_bool_literal(from)
}

fn extract_integer_list(
    expression: &StaticSpannedExpression,
    subset: &str,
) -> Result<Vec<i64>, String> {
    let StaticExpression::List { items } = &expression.node else {
        return Err(format!("{subset} subset requires numeric LIST values"));
    };
    items
        .iter()
        .map(extract_integer_literal)
        .collect::<Result<Vec<_>, _>>()
}

fn extract_selected_integer_subset_values(
    expression: &StaticSpannedExpression,
    subset: &str,
    item_alias: &str,
) -> Result<Vec<i64>, String> {
    let StaticExpression::Pipe { to, .. } = &expression.node else {
        return Err(format!("{subset} subset requires filtered pipe"));
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err(format!("{subset} subset requires `List/retain(...)`"));
    };
    if !path_matches(path, &["List", "retain"]) {
        return Err(format!("{subset} subset requires `List/retain(...)`"));
    }
    let predicate = find_named_argument(arguments, "if")
        .ok_or_else(|| format!("{subset} subset requires `List/retain(..., if: ...)`"))?;
    extract_when_selected_integer_subset_values(predicate, subset, item_alias)
}

fn extract_when_selected_integer_subset_values(
    expression: &StaticSpannedExpression,
    subset: &str,
    item_alias: &str,
) -> Result<Vec<i64>, String> {
    let StaticExpression::Pipe { to, .. } = &expression.node else {
        return Err(format!("{subset} subset requires `WHEN` predicate pipe"));
    };
    let StaticExpression::When { arms } = &to.node else {
        return Err(format!("{subset} subset requires `WHEN` predicate"));
    };
    let true_arm = arms
        .iter()
        .find(|arm| matches_bool_pattern(&arm.pattern, true))
        .ok_or_else(|| format!("{subset} subset requires a `True => ...` arm"))?;
    let false_arm = arms
        .iter()
        .find(|arm| matches_bool_pattern(&arm.pattern, false))
        .ok_or_else(|| format!("{subset} subset requires a `False => True` arm"))?;
    if !matches!(extract_bool_literal(&false_arm.body), Ok(true)) {
        return Err(format!("{subset} subset requires `False => True`"));
    }
    extract_disjunctive_alias_equality_values(&true_arm.body, subset, item_alias)
}

fn extract_disjunctive_alias_equality_values(
    expression: &StaticSpannedExpression,
    subset: &str,
    item_alias: &str,
) -> Result<Vec<i64>, String> {
    match &expression.node {
        StaticExpression::Pipe { from, to } => {
            let mut values = extract_disjunctive_alias_equality_values(from, subset, item_alias)?;
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Err(format!("{subset} subset requires `Bool/or(that: ...)`"));
            };
            if !path_matches(path, &["Bool", "or"]) {
                return Err(format!("{subset} subset requires `Bool/or(that: ...)`"));
            }
            let rhs = find_named_argument(arguments, "that")
                .ok_or_else(|| format!("{subset} subset requires `Bool/or(that: ...)`"))?;
            values.extend(extract_disjunctive_alias_equality_values(
                rhs, subset, item_alias,
            )?);
            Ok(values)
        }
        StaticExpression::Comparator(boon::parser::static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        }) => extract_alias_equality_value(operand_a, operand_b, item_alias)
            .or_else(|_| extract_alias_equality_value(operand_b, operand_a, item_alias))
            .map(|value| vec![value]),
        _ => Err(format!(
            "{subset} subset requires equality checks joined with Bool/or"
        )),
    }
}

fn extract_alias_equality_value(
    alias_expression: &StaticSpannedExpression,
    value_expression: &StaticSpannedExpression,
    item_alias: &str,
) -> Result<i64, String> {
    ensure_alias_name(alias_expression, item_alias)
        .map_err(|_| format!("subset requires `{item_alias} == <integer>` checks"))?;
    extract_integer_literal(value_expression)
        .map_err(|_| format!("subset requires `{item_alias} == <integer>` checks"))
}

fn matches_bool_pattern(
    pattern: &boon::parser::static_expression::Pattern,
    expected: bool,
) -> bool {
    matches!(
        pattern,
        boon::parser::static_expression::Pattern::Literal(
            boon::parser::static_expression::Literal::Tag(tag)
        ) if (expected && tag.as_str() == "True") || (!expected && tag.as_str() == "False")
    )
}

fn extract_bool_literal(expression: &StaticSpannedExpression) -> Result<bool, String> {
    match &expression.node {
        StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(tag))
            if tag.as_str() == "True" =>
        {
            Ok(true)
        }
        StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(tag))
            if tag.as_str() == "False" =>
        {
            Ok(false)
        }
        _ => Err("subset requires `True` or `False`".to_string()),
    }
}

define_source_parsed_entrypoint!(
    try_lower_list_map_external_dep,
    try_lower_list_map_external_dep_from_expressions,
    ListMapExternalDepProgram
);

fn try_lower_list_map_external_dep_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ListMapExternalDepProgram, String> {
    lower_bindings_group_typed_program(
        expressions,
        &LIST_BOOL_TOGGLE_SEMANTIC_BINDINGS_GROUP,
        "external_mode_mapped_items_document",
        |program| match program {
            LoweredProgram::ListMapExternalDep(program) => Some(program),
            _ => None,
        },
    )
}

#[derive(Clone, Copy)]
struct AppendListValidationSpec<'a> {
    subset: &'static str,
    required_paths: &'a [&'a [&'a str]],
    required_functions: &'a [&'a str],
    alias_paths: &'a [&'a [&'a str]],
    function_call_paths: &'a [&'a [&'a str]],
    text_fragments: &'a [&'a str],
    require_latest: bool,
    require_then: bool,
    require_when: bool,
}

impl LoweringSubset for AppendListValidationSpec<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.subset
    }
}

fn require_append_list_validation(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    spec: &AppendListValidationSpec<'_>,
) -> Result<(), String> {
    require_top_level_bindings(bindings, spec.subset, &["store", "document"])?;
    for path in spec.required_paths {
        require_binding_at_path(bindings, spec.subset, path)?;
    }
    if !spec.required_functions.is_empty() {
        require_top_level_functions(expressions, spec.subset, spec.required_functions)?;
    }
    if !spec.alias_paths.is_empty() {
        require_alias_paths(expressions, spec.subset, spec.alias_paths)?;
    }
    if !spec.function_call_paths.is_empty() {
        require_function_call_paths(expressions, spec.subset, spec.function_call_paths)?;
    }
    if spec.require_latest && !contains_latest_expression(expressions) {
        return Err(format!("{} subset requires LATEST expression", spec.subset));
    }
    if spec.require_then && !contains_then_expression(expressions) {
        return Err(format!("{} subset requires THEN expression", spec.subset));
    }
    if spec.require_when && !contains_when_expression(expressions) {
        return Err(format!("{} subset requires WHEN expressions", spec.subset));
    }
    require_text_fragments(expressions, spec.subset, spec.text_fragments)
}

fn validated_append_list_bindings<'a>(
    expressions: &'a [StaticSpannedExpression],
    spec: &AppendListValidationSpec<'_>,
) -> Result<BTreeMap<String, &'a StaticSpannedExpression>, String> {
    lower_with_bindings(expressions, |bindings| {
        require_append_list_validation(expressions, &bindings, spec)?;
        Ok(bindings)
    })
}

#[derive(Clone, Copy)]
struct StructuralValidationSpec<'a> {
    subset: &'static str,
    top_level_bindings: &'a [&'a str],
    required_paths: &'a [&'a [&'a str]],
    hold_paths: &'a [&'a [&'a str]],
    required_functions: &'a [&'a str],
    alias_paths: &'a [&'a [&'a str]],
    function_call_paths: &'a [&'a [&'a str]],
    text_fragments: &'a [&'a str],
    require_hold: bool,
    require_latest: bool,
    require_then: bool,
    require_when: bool,
    require_while: bool,
}

impl LoweringSubset for StructuralValidationSpec<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.subset
    }
}

fn require_structural_validation(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    spec: &StructuralValidationSpec<'_>,
) -> Result<(), String> {
    if !spec.top_level_bindings.is_empty() {
        require_top_level_bindings(bindings, spec.subset, spec.top_level_bindings)?;
    }
    for path in spec.required_paths {
        require_binding_at_path(bindings, spec.subset, path)?;
    }
    for path in spec.hold_paths {
        require_hold_binding_at_path(bindings, spec.subset, path)?;
    }
    if !spec.required_functions.is_empty() {
        require_top_level_functions(expressions, spec.subset, spec.required_functions)?;
    }
    if !spec.alias_paths.is_empty() {
        require_alias_paths(expressions, spec.subset, spec.alias_paths)?;
    }
    if !spec.function_call_paths.is_empty() {
        require_function_call_paths(expressions, spec.subset, spec.function_call_paths)?;
    }
    if spec.require_hold && !contains_hold_expression(expressions) {
        return Err(format!("{} subset requires HOLD expressions", spec.subset));
    }
    if spec.require_latest && !contains_latest_expression(expressions) {
        return Err(format!("{} subset requires LATEST expression", spec.subset));
    }
    if spec.require_then && !contains_then_expression(expressions) {
        return Err(format!("{} subset requires THEN expressions", spec.subset));
    }
    if spec.require_when && !contains_when_expression(expressions) {
        return Err(format!("{} subset requires WHEN expressions", spec.subset));
    }
    if spec.require_while && !contains_while_expression(expressions) {
        return Err(format!("{} subset requires WHILE expressions", spec.subset));
    }
    require_text_fragments(expressions, spec.subset, spec.text_fragments)
}

fn lower_with_bindings<'a, T>(
    expressions: &'a [StaticSpannedExpression],
    build_output: impl FnOnce(BTreeMap<String, &'a StaticSpannedExpression>) -> Result<T, String>,
) -> Result<T, String> {
    let bindings = top_level_bindings(expressions);
    build_output(bindings)
}

fn validated_top_level_bindings<'a>(
    expressions: &'a [StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
) -> Result<BTreeMap<String, &'a StaticSpannedExpression>, String> {
    lower_with_bindings(expressions, |bindings| {
        require_structural_validation(expressions, &bindings, validation)?;
        Ok(bindings)
    })
}

fn lower_generic_host_view_from_bindings(
    expressions: &[StaticSpannedExpression],
    sink_bindings: &[(&str, SinkPortId)],
    source_bindings: &[(&str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
) -> Result<HostViewIr, String> {
    lower_generic_host_view(
        expressions,
        sink_bindings,
        source_bindings,
        view_site,
        function_instance,
    )
}

fn lower_validated_generic_host_view_from_bindings(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    validation: &StructuralValidationSpec<'_>,
    sink_bindings: &[(&str, SinkPortId)],
    source_bindings: &[(&str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
) -> Result<HostViewIr, String> {
    require_structural_validation(expressions, bindings, validation)?;
    lower_generic_host_view_from_bindings(
        expressions,
        sink_bindings,
        source_bindings,
        view_site,
        function_instance,
    )
}

fn lower_validated_generic_host_view(
    expressions: &[StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
    sink_bindings: &[(&str, SinkPortId)],
    source_bindings: &[(&str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
) -> Result<HostViewIr, String> {
    lower_with_bindings(expressions, |bindings| {
        lower_validated_generic_host_view_from_bindings(
            expressions,
            &bindings,
            validation,
            sink_bindings,
            source_bindings,
            view_site,
            function_instance,
        )
    })
}

#[derive(Clone, Copy)]
struct GenericHostSurfaceConfig<'a> {
    validation: StructuralValidationSpec<'a>,
    sink_bindings: &'a [(&'a str, SinkPortId)],
    source_bindings: &'a [(&'a str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
}

impl LoweringSubset for GenericHostSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

fn lower_generic_host_surface(
    expressions: &[StaticSpannedExpression],
    config: &GenericHostSurfaceConfig<'_>,
) -> Result<HostViewIr, String> {
    lower_validated_generic_host_view(
        expressions,
        &config.validation,
        config.sink_bindings,
        config.source_bindings,
        config.view_site,
        config.function_instance,
    )
}

fn lower_generic_host_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: GenericHostSurfaceConfig<'static>,
) -> Result<HostViewIr, String> {
    lower_generic_host_surface(expressions, &config)
}

#[derive(Clone, Copy)]
struct TimerSourceChildConfig {
    port: SourcePortId,
    interval_ms: u32,
}

#[derive(Clone, Copy)]
struct GenericHostIrSurfaceConfig<'a> {
    validation: StructuralValidationSpec<'a>,
    sink_bindings: &'a [(&'a str, SinkPortId)],
    source_bindings: &'a [(&'a str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    timer_source_children: &'a [TimerSourceChildConfig],
}

impl LoweringSubset for GenericHostIrSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

fn lower_generic_host_ir_from_bindings(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &GenericHostIrSurfaceConfig<'_>,
) -> Result<HostViewIr, String> {
    let mut host_view = lower_validated_generic_host_view_from_bindings(
        expressions,
        bindings,
        &config.validation,
        config.sink_bindings,
        config.source_bindings,
        config.view_site,
        config.function_instance,
    )?;
    let mut view_site_counter = next_free_view_site(&host_view);
    for timer_source_child in config.timer_source_children {
        append_timer_source_child(
            &mut host_view,
            config.function_instance,
            timer_source_child.port,
            timer_source_child.interval_ms,
            &mut view_site_counter,
        );
    }
    Ok(host_view)
}

fn lower_generic_host_ir_surface(
    expressions: &[StaticSpannedExpression],
    config: &GenericHostIrSurfaceConfig<'_>,
) -> Result<HostViewIr, String> {
    lower_with_bindings(expressions, |bindings| {
        lower_generic_host_ir_from_bindings(expressions, &bindings, config)
    })
}

fn lower_bindings_with_host_view<'a, C, T>(
    expressions: &'a [StaticSpannedExpression],
    build_host_view: impl FnOnce(
        &BTreeMap<String, &'a StaticSpannedExpression>,
    ) -> Result<(C, HostViewIr), String>,
    build_output: impl FnOnce(
        BTreeMap<String, &'a StaticSpannedExpression>,
        C,
        HostViewIr,
    ) -> Result<T, String>,
) -> Result<T, String> {
    lower_with_bindings(expressions, |bindings| {
        let (context, host_view) = build_host_view(&bindings)?;
        build_output(bindings, context, host_view)
    })
}

fn lower_bindings_with_generic_host_ir<'a, C, T>(
    expressions: &'a [StaticSpannedExpression],
    build_context: impl FnOnce(&BTreeMap<String, &'a StaticSpannedExpression>) -> Result<C, String>,
    build_host_view: impl FnOnce(
        &BTreeMap<String, &'a StaticSpannedExpression>,
        &C,
    ) -> Result<HostViewIr, String>,
    build_output: impl FnOnce(
        BTreeMap<String, &'a StaticSpannedExpression>,
        C,
        HostViewIr,
    ) -> Result<T, String>,
) -> Result<T, String> {
    lower_bindings_with_host_view(
        expressions,
        |bindings| {
            let context = build_context(bindings)?;
            let host_view = build_host_view(bindings, &context)?;
            Ok((context, host_view))
        },
        build_output,
    )
}

fn lower_bindings_single_sink_value_program<T>(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    sink_binding: &str,
    sink: SinkPortId,
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    sink_value: KernelValue,
    build_program: impl FnOnce(HostViewIr, BTreeMap<SinkPortId, KernelValue>) -> T,
) -> Result<T, String> {
    let sink_bindings = [(sink_binding, sink)];
    lower_generic_host_surface_with_sink_values_from_bindings_program(
        expressions,
        bindings,
        &GenericHostSinkValuesFromBindingsConfig {
            sink_bindings: &sink_bindings,
            source_bindings: &[],
            view_site,
            function_instance,
            sink_values: [(sink, sink_value)],
        },
        build_program,
    )
}

fn lower_generic_host_ir_program_surface(
    expressions: &[StaticSpannedExpression],
    ir: IrProgram,
    config: &GenericHostIrSurfaceConfig<'_>,
) -> Result<(IrProgram, HostViewIr), String> {
    let host_view = lower_generic_host_ir_surface(expressions, config)?;
    Ok((ir, host_view))
}

fn lower_generic_host_ir_program<T>(
    expressions: &[StaticSpannedExpression],
    ir: IrProgram,
    config: &GenericHostIrSurfaceConfig<'_>,
    build_program: impl FnOnce(IrProgram, HostViewIr) -> T,
) -> Result<T, String> {
    let (ir, host_view) = lower_generic_host_ir_program_surface(expressions, ir, config)?;
    Ok(build_program(ir, host_view))
}

fn lower_generic_host_ir_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: GenericHostIrProgramSurfaceConfig<'static>,
) -> Result<LoweredProgram, String> {
    let (ir, host_view) =
        lower_generic_host_ir_program_surface(expressions, (config.build_ir)(), &config.surface)?;
    Ok(build_ir_host_view_lowered_program(
        ir,
        host_view,
        config.program,
    ))
}

fn lower_form_runtime_ir_program<T>(
    expressions: &[StaticSpannedExpression],
    ir: IrProgram,
    validation: StructuralValidationSpec<'_>,
    sink_bindings: &[(&str, SinkPortId)],
    source_bindings: &[(&str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    timer_source_children: &[TimerSourceChildConfig],
    build_program: impl FnOnce(IrProgram, HostViewIr) -> T,
) -> Result<T, String> {
    lower_generic_host_ir_program(
        expressions,
        ir,
        &GenericHostIrSurfaceConfig {
            validation,
            sink_bindings,
            source_bindings,
            view_site,
            function_instance,
            timer_source_children,
        },
        build_program,
    )
}

#[derive(Clone, Copy)]
struct FormRuntimeProgramConfig<'a> {
    validation: StructuralValidationSpec<'a>,
    sink_bindings: &'a [(&'a str, SinkPortId)],
    source_bindings: &'a [(&'a str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    timer_source_children: &'a [TimerSourceChildConfig],
}

#[derive(Clone, Copy)]
struct FormRuntimeSurfaceConfig<'a> {
    config: FormRuntimeProgramConfig<'a>,
    build_ir: fn() -> IrProgram,
    program: FormRuntimeLoweredProgramSpec,
}

impl LoweringSubset for FormRuntimeSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.config.validation.lowering_subset()
    }
}

#[derive(Clone, Copy)]
enum FormRuntimeLoweredProgramSpec {
    TemperatureConverter,
    FlightBooker,
    Timer,
}

struct FormRuntimeProgramLowering {
    ir: IrProgram,
    host_view: HostViewIr,
}

impl FormRuntimeProgramLowering {
    fn into_temperature_converter_program(self) -> TemperatureConverterProgram {
        TemperatureConverterProgram {
            ir: self.ir,
            host_view: self.host_view,
            title_sink: SinkPortId(1800),
            celsius_input_sink: SinkPortId(1801),
            fahrenheit_input_sink: SinkPortId(1802),
            celsius_label_sink: SinkPortId(1803),
            equals_label_sink: SinkPortId(1804),
            fahrenheit_label_sink: SinkPortId(1805),
            celsius_change_port: SourcePortId(1800),
            celsius_key_down_port: SourcePortId(1801),
            fahrenheit_change_port: SourcePortId(1802),
            fahrenheit_key_down_port: SourcePortId(1803),
        }
    }

    fn into_flight_booker_program(self) -> FlightBookerProgram {
        FlightBookerProgram {
            ir: self.ir,
            host_view: self.host_view,
            title_sink: SinkPortId(1900),
            selected_flight_type_sink: SinkPortId(1901),
            departure_input_sink: SinkPortId(1902),
            return_input_sink: SinkPortId(1903),
            return_input_disabled_sink: SinkPortId(1904),
            book_button_disabled_sink: SinkPortId(1905),
            booked_sink: SinkPortId(1906),
            flight_type_change_port: SourcePortId(1900),
            departure_change_port: SourcePortId(1901),
            return_change_port: SourcePortId(1902),
            book_press_port: SourcePortId(1903),
        }
    }

    fn into_timer_program(self) -> TimerProgram {
        TimerProgram {
            ir: self.ir,
            host_view: self.host_view,
            title_sink: SinkPortId(1950),
            elapsed_title_sink: SinkPortId(1951),
            progress_percent_sink: SinkPortId(1952),
            elapsed_value_sink: SinkPortId(1953),
            duration_title_sink: SinkPortId(1954),
            duration_slider_sink: SinkPortId(1955),
            duration_value_sink: SinkPortId(1956),
            duration_change_port: SourcePortId(1950),
            reset_press_port: SourcePortId(1951),
            tick_port: SourcePortId(1952),
        }
    }
}

fn build_form_runtime_lowered_program(
    program: FormRuntimeProgramLowering,
    spec: FormRuntimeLoweredProgramSpec,
) -> LoweredProgram {
    match spec {
        FormRuntimeLoweredProgramSpec::TemperatureConverter => {
            LoweredProgram::TemperatureConverter(program.into_temperature_converter_program())
        }
        FormRuntimeLoweredProgramSpec::FlightBooker => {
            LoweredProgram::FlightBooker(program.into_flight_booker_program())
        }
        FormRuntimeLoweredProgramSpec::Timer => LoweredProgram::Timer(program.into_timer_program()),
    }
}

fn lower_form_runtime_program(
    expressions: &[StaticSpannedExpression],
    ir: IrProgram,
    config: &FormRuntimeProgramConfig<'_>,
) -> Result<FormRuntimeProgramLowering, String> {
    lower_form_runtime_ir_program(
        expressions,
        ir,
        config.validation.clone(),
        config.sink_bindings,
        config.source_bindings,
        config.view_site,
        config.function_instance,
        config.timer_source_children,
        |ir, host_view| FormRuntimeProgramLowering { ir, host_view },
    )
}

fn lower_form_runtime_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: FormRuntimeSurfaceConfig<'static>,
) -> Result<LoweredProgram, String> {
    let lowering = lower_form_runtime_program(expressions, (config.build_ir)(), &config.config)?;
    Ok(build_form_runtime_lowered_program(lowering, config.program))
}

#[derive(Clone)]
struct GenericHostSinkValuesSurfaceConfig<'a, I>
where
    I: Clone + IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    validation: StructuralValidationSpec<'a>,
    sink_bindings: &'a [(&'a str, SinkPortId)],
    source_bindings: &'a [(&'a str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    sink_values: I,
}

impl<I> LoweringSubset for GenericHostSinkValuesSurfaceConfig<'_, I>
where
    I: Clone + IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

#[derive(Clone, Copy)]
enum StaticKernelValue {
    Text(&'static str),
}

trait IntoSinkValueEntry {
    fn into_sink_value_entry(self) -> (SinkPortId, KernelValue);
}

impl IntoSinkValueEntry for (SinkPortId, KernelValue) {
    fn into_sink_value_entry(self) -> (SinkPortId, KernelValue) {
        self
    }
}

impl IntoSinkValueEntry for (SinkPortId, StaticKernelValue) {
    fn into_sink_value_entry(self) -> (SinkPortId, KernelValue) {
        let (sink, value) = self;
        let value = match value {
            StaticKernelValue::Text(text) => KernelValue::from(text),
        };
        (sink, value)
    }
}

fn collect_sink_value_entries<I>(sink_values: I) -> BTreeMap<SinkPortId, KernelValue>
where
    I: IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    sink_values
        .into_iter()
        .map(IntoSinkValueEntry::into_sink_value_entry)
        .collect()
}

fn lower_generic_host_surface_with_sink_values<I>(
    expressions: &[StaticSpannedExpression],
    config: &GenericHostSinkValuesSurfaceConfig<'_, I>,
) -> Result<(HostViewIr, BTreeMap<SinkPortId, KernelValue>), String>
where
    I: Clone + IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    lower_validated_generic_host_view_with_sink_values(
        expressions,
        &config.validation,
        config.sink_bindings,
        config.source_bindings,
        config.view_site,
        config.function_instance,
        config.sink_values.clone(),
    )
}

fn lower_generic_host_surface_with_sink_values_owned<I>(
    expressions: &[StaticSpannedExpression],
    config: GenericHostSinkValuesSurfaceConfig<'static, I>,
) -> Result<(HostViewIr, BTreeMap<SinkPortId, KernelValue>), String>
where
    I: Clone + IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    lower_generic_host_surface_with_sink_values(expressions, &config)
}

struct GenericHostSinkValuesFromBindingsConfig<'a, I>
where
    I: Clone + IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    sink_bindings: &'a [(&'a str, SinkPortId)],
    source_bindings: &'a [(&'a str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    sink_values: I,
}

fn lower_generic_host_surface_with_sink_values_from_bindings<I>(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &GenericHostSinkValuesFromBindingsConfig<'_, I>,
) -> Result<(HostViewIr, BTreeMap<SinkPortId, KernelValue>), String>
where
    I: Clone + IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    lower_generic_host_view_with_sink_values_from_bindings(
        expressions,
        bindings,
        config.sink_bindings,
        config.source_bindings,
        config.view_site,
        config.function_instance,
        config.sink_values.clone(),
    )
}

fn lower_generic_host_surface_with_sink_values_from_bindings_program<I, T>(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &GenericHostSinkValuesFromBindingsConfig<'_, I>,
    build_program: impl FnOnce(HostViewIr, BTreeMap<SinkPortId, KernelValue>) -> T,
) -> Result<T, String>
where
    I: Clone + IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    let (host_view, sink_values) =
        lower_generic_host_surface_with_sink_values_from_bindings(expressions, bindings, config)?;
    Ok(build_program(host_view, sink_values))
}

fn lower_generic_host_view_with_sink_values_from_bindings<I>(
    expressions: &[StaticSpannedExpression],
    _bindings: &BTreeMap<String, &StaticSpannedExpression>,
    sink_bindings: &[(&str, SinkPortId)],
    source_bindings: &[(&str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    sink_values: I,
) -> Result<(HostViewIr, BTreeMap<SinkPortId, KernelValue>), String>
where
    I: IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    Ok((
        lower_generic_host_view_from_bindings(
            expressions,
            sink_bindings,
            source_bindings,
            view_site,
            function_instance,
        )?,
        collect_sink_value_entries(sink_values),
    ))
}

fn lower_validated_generic_host_view_with_sink_values<I>(
    expressions: &[StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
    sink_bindings: &[(&str, SinkPortId)],
    source_bindings: &[(&str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    sink_values: I,
) -> Result<(HostViewIr, BTreeMap<SinkPortId, KernelValue>), String>
where
    I: IntoIterator,
    I::Item: IntoSinkValueEntry,
{
    Ok((
        lower_validated_generic_host_view(
            expressions,
            validation,
            sink_bindings,
            source_bindings,
            view_site,
            function_instance,
        )?,
        collect_sink_value_entries(sink_values),
    ))
}

fn lower_validated_checkbox_list_document(
    expressions: &[StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
    function_instance: FunctionInstanceId,
    root_view_site: ViewSiteId,
    container_view_site: ViewSiteId,
    container_kind: CheckboxListDocumentContainerKind,
    prefix_children: &[CheckboxListDocumentChildConfig<'_>],
    rows_container_view_site: Option<ViewSiteId>,
    rows: &MappedCheckboxRowsConfig<'_>,
    suffix_children: &[CheckboxListDocumentChildConfig<'_>],
) -> Result<HostViewIr, String> {
    let _bindings = validated_top_level_bindings(expressions, validation)?;
    Ok(lower_checkbox_list_document(
        function_instance,
        root_view_site,
        container_view_site,
        container_kind,
        prefix_children,
        rows_container_view_site,
        rows,
        suffix_children,
    ))
}

#[derive(Clone, Copy)]
struct CheckboxListSurfaceConfig<'a, 'b> {
    validation: StructuralValidationSpec<'a>,
    function_instance: FunctionInstanceId,
    root_view_site: ViewSiteId,
    container_view_site: ViewSiteId,
    container_kind: CheckboxListDocumentContainerKind,
    prefix_children: &'b [CheckboxListDocumentChildConfig<'b>],
    rows_container_view_site: Option<ViewSiteId>,
    rows: &'b MappedCheckboxRowsConfig<'b>,
    suffix_children: &'b [CheckboxListDocumentChildConfig<'b>],
}

impl LoweringSubset for CheckboxListSurfaceConfig<'_, '_> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

fn lower_checkbox_list_surface(
    expressions: &[StaticSpannedExpression],
    config: &CheckboxListSurfaceConfig<'_, '_>,
) -> Result<HostViewIr, String> {
    lower_validated_checkbox_list_document(
        expressions,
        &config.validation,
        config.function_instance,
        config.root_view_site,
        config.container_view_site,
        config.container_kind.clone(),
        config.prefix_children,
        config.rows_container_view_site,
        config.rows,
        config.suffix_children,
    )
}

fn lower_checkbox_list_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: CheckboxListSurfaceConfig<'static, 'static>,
) -> Result<HostViewIr, String> {
    lower_checkbox_list_surface(expressions, &config)
}

fn lower_validated_flat_stripe_document(
    expressions: &[StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
    config: &FlatStripeDocumentConfig<'_>,
) -> Result<HostViewIr, String> {
    let _bindings = validated_top_level_bindings(expressions, validation)?;
    Ok(lower_flat_stripe_document(config))
}

fn lower_validated_flat_stripe_document_from_bindings(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    validation: &StructuralValidationSpec<'_>,
    config: &FlatStripeDocumentConfig<'_>,
) -> Result<HostViewIr, String> {
    require_structural_validation(expressions, bindings, validation)?;
    Ok(lower_flat_stripe_document(config))
}

#[derive(Clone, Copy)]
struct FlatStripeSurfaceConfig<'a, 'b> {
    validation: StructuralValidationSpec<'a>,
    document: FlatStripeDocumentConfig<'b>,
}

impl LoweringSubset for FlatStripeSurfaceConfig<'_, '_> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

#[derive(Clone, Copy)]
struct FlatStripeHostOnlySurfaceConfig<'a, 'b> {
    validation: StructuralValidationSpec<'a>,
    document: FlatStripeDocumentConfig<'b>,
}

impl LoweringSubset for FlatStripeHostOnlySurfaceConfig<'_, '_> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

fn lower_flat_stripe_surface(
    expressions: &[StaticSpannedExpression],
    config: &FlatStripeSurfaceConfig<'_, '_>,
) -> Result<HostViewIr, String> {
    lower_validated_flat_stripe_document(expressions, &config.validation, &config.document)
}

fn lower_flat_stripe_surface_program_from_bindings<T>(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    config: &FlatStripeSurfaceConfig<'_, '_>,
    build_program: impl FnOnce(HostViewIr) -> T,
) -> Result<T, String> {
    let host_view = lower_validated_flat_stripe_document_from_bindings(
        expressions,
        bindings,
        &config.validation,
        &config.document,
    )?;
    Ok(build_program(host_view))
}

fn lower_flat_stripe_host_only_surface(
    expressions: &[StaticSpannedExpression],
    config: FlatStripeHostOnlySurfaceConfig<'_, '_>,
) -> Result<HostViewIr, String> {
    lower_flat_stripe_surface(
        expressions,
        &FlatStripeSurfaceConfig {
            validation: config.validation,
            document: config.document,
        },
    )
}

fn lower_flat_stripe_host_only_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: FlatStripeHostOnlySurfaceConfig<'static, 'static>,
) -> Result<HostViewIr, String> {
    lower_flat_stripe_host_only_surface(expressions, config)
}

#[derive(Clone)]
struct AppendListFlatStripeDocumentConfig<'a> {
    function_instance: FunctionInstanceId,
    root_view_site: ViewSiteId,
    stripe_view_site: ViewSiteId,
    title: Option<(ViewSiteId, SinkPortId)>,
    input_view_site: ViewSiteId,
    input_sink: SinkPortId,
    placeholder: &'a str,
    input_change_port: SourcePortId,
    input_key_down_port: SourcePortId,
    focus_on_mount: bool,
    labels: &'a [(ViewSiteId, SinkPortId)],
    mapped_list: MappedLabelListConfig,
}

fn lower_append_list_flat_stripe_document(
    config: &AppendListFlatStripeDocumentConfig<'_>,
) -> HostViewIr {
    let mut children = Vec::new();
    if let Some((view_site, sink)) = config.title {
        children.push(FlatStripeDocumentChildConfig::Label { view_site, sink });
    }
    children.push(FlatStripeDocumentChildConfig::TextInput {
        view_site: config.input_view_site,
        value_sink: config.input_sink,
        placeholder: config.placeholder,
        change_port: config.input_change_port,
        key_down_port: config.input_key_down_port,
        focus_on_mount: config.focus_on_mount,
    });
    children.extend(config.labels.iter().map(|(view_site, sink)| {
        FlatStripeDocumentChildConfig::Label {
            view_site: *view_site,
            sink: *sink,
        }
    }));
    children.push(FlatStripeDocumentChildConfig::MappedLabelList(
        config.mapped_list.clone(),
    ));

    lower_flat_stripe_document(&FlatStripeDocumentConfig {
        function_instance: config.function_instance,
        root_view_site: config.root_view_site,
        stripe_view_site: config.stripe_view_site,
        children: &children,
    })
}

#[derive(Clone)]
enum AppendListHostViewConfig<'a> {
    FlatStripe(AppendListFlatStripeDocumentConfig<'a>),
    TitledColumn(TitledColumnDocumentConfig),
}

fn lower_append_list_host_view(config: AppendListHostViewConfig<'_>) -> HostViewIr {
    match config {
        AppendListHostViewConfig::FlatStripe(config) => {
            lower_append_list_flat_stripe_document(&config)
        }
        AppendListHostViewConfig::TitledColumn(config) => {
            lower_titled_column_document_from_config(config)
        }
    }
}

#[derive(Clone)]
struct AppendListSurfaceConfig<'a> {
    validation: AppendListValidationSpec<'a>,
    runtime: AppendListRuntimeConfig<'a>,
    host_view: AppendListHostViewConfig<'a>,
}

impl LoweringSubset for AppendListSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

fn lower_append_list_surface(
    expressions: &[StaticSpannedExpression],
    config: AppendListSurfaceConfig<'_>,
) -> Result<(IrProgram, HostViewIr), String> {
    let runtime =
        lower_append_list_family_runtime(expressions, &config.validation, &config.runtime)?;
    Ok((
        lower_append_list_ephemeral_program(runtime),
        lower_append_list_host_view(config.host_view),
    ))
}

fn lower_append_list_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: AppendListSurfaceConfig<'static>,
) -> Result<(IrProgram, HostViewIr), String> {
    lower_append_list_surface(expressions, config)
}

#[derive(Clone, Copy)]
enum StaticHostViewKind<'a> {
    StripeLayout {
        direction: HostStripeDirection,
        gap_px: u32,
        padding_px: Option<u32>,
        width: Option<HostWidth>,
        align_cross: Option<HostCrossAlign>,
    },
    StyledLabel {
        sink: SinkPortId,
        font_size_px: Option<u32>,
        bold: bool,
        color: Option<&'a str>,
    },
    StyledTextInput {
        value_sink: SinkPortId,
        placeholder: &'a str,
        change_port: SourcePortId,
        key_down_port: SourcePortId,
        focus_on_mount: bool,
        disabled_sink: Option<SinkPortId>,
        width: Option<HostWidth>,
    },
    StyledButton {
        label: &'a str,
        press_port: SourcePortId,
        disabled_sink: Option<SinkPortId>,
        width: Option<HostWidth>,
        padding_px: Option<u32>,
        rounded_fully: bool,
        background: Option<&'a str>,
        background_sink: Option<SinkPortId>,
        active_background: Option<&'a str>,
        outline_sink: Option<SinkPortId>,
        active_outline: Option<&'a str>,
    },
    StyledActionLabel {
        sink: SinkPortId,
        press_port: SourcePortId,
        width: Option<HostWidth>,
        bold_sink: Option<SinkPortId>,
    },
}

#[derive(Clone, Copy)]
struct StaticHostViewNode<'a> {
    retained_key: RetainedNodeKey,
    kind: StaticHostViewKind<'a>,
    children: &'a [StaticHostViewNode<'a>],
}

#[derive(Clone, Copy)]
struct TitledColumnDocumentConfig {
    function_instance: FunctionInstanceId,
    root_view_site: ViewSiteId,
    stripe_view_site: ViewSiteId,
    gap_px: u32,
    padding_px: Option<u32>,
    width: Option<HostWidth>,
    title_view_site: ViewSiteId,
    title_sink: SinkPortId,
    title_font_size_px: u32,
    body_children: &'static [StaticHostViewNode<'static>],
}

#[derive(Clone)]
struct TitledColumnSurfaceConfig<'a> {
    validation: StructuralValidationSpec<'a>,
    document: TitledColumnDocumentConfig,
}

impl LoweringSubset for TitledColumnSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.validation.lowering_subset()
    }
}

fn lower_titled_column_document_from_config(config: TitledColumnDocumentConfig) -> HostViewIr {
    lower_titled_column_document(
        config.function_instance,
        config.root_view_site,
        config.stripe_view_site,
        config.gap_px,
        config.padding_px,
        config.width,
        config.title_view_site,
        config.title_sink,
        config.title_font_size_px,
        lower_static_host_view_nodes(config.body_children),
    )
}

fn lower_static_host_view_nodes(nodes: &[StaticHostViewNode<'_>]) -> Vec<HostViewNode> {
    nodes.iter().map(lower_static_host_view_node).collect()
}

fn lower_static_host_view_node(node: &StaticHostViewNode<'_>) -> HostViewNode {
    HostViewNode {
        retained_key: node.retained_key,
        kind: lower_static_host_view_kind(&node.kind),
        children: lower_static_host_view_nodes(node.children),
    }
}

fn lower_static_host_view_kind(kind: &StaticHostViewKind<'_>) -> HostViewKind {
    match kind {
        StaticHostViewKind::StripeLayout {
            direction,
            gap_px,
            padding_px,
            width,
            align_cross,
        } => styled_stripe_layout(*direction, *gap_px, *padding_px, *width, *align_cross),
        StaticHostViewKind::StyledLabel {
            sink,
            font_size_px,
            bold,
            color,
        } => styled_label(*sink, *font_size_px, *bold, *color),
        StaticHostViewKind::StyledTextInput {
            value_sink,
            placeholder,
            change_port,
            key_down_port,
            focus_on_mount,
            disabled_sink,
            width,
        } => styled_text_input(
            *value_sink,
            placeholder,
            *change_port,
            *key_down_port,
            *focus_on_mount,
            *disabled_sink,
            *width,
        ),
        StaticHostViewKind::StyledButton {
            label,
            press_port,
            disabled_sink,
            width,
            padding_px,
            rounded_fully,
            background,
            background_sink,
            active_background,
            outline_sink,
            active_outline,
        } => styled_button(
            label,
            *press_port,
            *disabled_sink,
            *width,
            *padding_px,
            *rounded_fully,
            *background,
            *background_sink,
            *active_background,
            *outline_sink,
            *active_outline,
        ),
        StaticHostViewKind::StyledActionLabel {
            sink,
            press_port,
            width,
            bold_sink,
        } => styled_action_label(*sink, *press_port, *width, *bold_sink),
    }
}

struct ValidationOnlyTypedConfig<T> {
    validation: StructuralValidationSpec<'static>,
    build_output: fn() -> T,
}

fn lower_validation_only_surface(
    expressions: &[StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
) -> Result<(), String> {
    let _bindings = validated_top_level_bindings(expressions, validation)?;
    Ok(())
}

fn lower_validation_only_typed_program<T>(
    expressions: &[StaticSpannedExpression],
    config: &ValidationOnlyTypedConfig<T>,
) -> Result<T, String> {
    lower_validation_only_surface(expressions, &config.validation)?;
    Ok((config.build_output)())
}

fn lower_validated_titled_column_document(
    expressions: &[StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
    config: TitledColumnDocumentConfig,
) -> Result<HostViewIr, String> {
    lower_validation_only_surface(expressions, validation)?;
    Ok(lower_titled_column_document_from_config(config))
}

fn lower_titled_column_surface(
    expressions: &[StaticSpannedExpression],
    config: TitledColumnSurfaceConfig<'_>,
) -> Result<HostViewIr, String> {
    lower_validated_titled_column_document(expressions, &config.validation, config.document)
}

fn lower_titled_column_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: TitledColumnSurfaceConfig<'static>,
) -> Result<HostViewIr, String> {
    lower_titled_column_surface(expressions, config)
}

#[derive(Clone, Copy)]
struct AppendListRuntimeConfig<'a> {
    title: Option<(&'a str, SinkPortId)>,
    input_change_port: SourcePortId,
    input_key_down_port: SourcePortId,
    input_sink: SinkPortId,
    count_sink: SinkPortId,
    count_prefix: &'a str,
    count_suffix: Option<&'a str>,
    derived_count_sinks: &'a [AppendListDerivedCountConfig<'a>],
    items_list_sink: SinkPortId,
    clear_press_port: Option<SourcePortId>,
    initial_items: &'a [&'a str],
    base_node_id: u32,
}

#[derive(Clone, Copy)]
struct AppendListDerivedCountConfig<'a> {
    list: AppendListDerivedListSpec,
    sink: SinkPortId,
    prefix: &'a str,
    suffix: Option<&'a str>,
}

#[derive(Clone, Copy)]
enum AppendListDerivedListSpec {
    RetainLiteralBool(bool),
}

struct AppendListRuntime {
    nodes: Vec<IrNode>,
}

fn lower_append_list_ephemeral_program(runtime: AppendListRuntime) -> IrProgram {
    IrProgram {
        nodes: runtime.nodes,
        functions: Vec::new(),
        persistence: Vec::new(),
    }
}

fn lower_append_list_family_runtime(
    expressions: &[StaticSpannedExpression],
    validation_spec: &AppendListValidationSpec<'_>,
    runtime_config: &AppendListRuntimeConfig<'_>,
) -> Result<AppendListRuntime, String> {
    let _bindings = validated_append_list_bindings(expressions, validation_spec)?;
    Ok(lower_append_list_runtime(runtime_config))
}

fn lower_append_list_runtime(config: &AppendListRuntimeConfig<'_>) -> AppendListRuntime {
    let mut next = config.base_node_id;
    let mut alloc = || {
        let id = NodeId(next);
        next += 1;
        id
    };

    let mut nodes = Vec::new();

    if let Some((title, sink)) = config.title {
        let title_literal = append_literal(&mut nodes, KernelValue::from(title), alloc().0);
        append_value_sink(&mut nodes, title_literal, sink, alloc().0);
    }

    let empty = append_literal(&mut nodes, KernelValue::from(""), alloc().0);
    let input_change = append_source_port(&mut nodes, config.input_change_port, alloc().0);
    let input_key_down = append_source_port(&mut nodes, config.input_key_down_port, alloc().0);
    let key = alloc();
    let entered_text = alloc();
    nodes.push(IrNode {
        id: entered_text,
        source_expr: None,
        kind: IrNodeKind::KeyDownText {
            input: input_key_down,
        },
    });
    let trimmed_text = append_trimmed_text(&mut nodes, entered_text, alloc().0);
    let skip = alloc();
    nodes.push(IrNode {
        id: skip,
        source_expr: None,
        kind: IrNodeKind::Skip,
    });
    let non_empty_text = append_non_empty_value_or_skip(
        &mut nodes,
        trimmed_text,
        empty,
        trimmed_text,
        alloc().0,
        alloc().0,
        skip,
    );
    let appended_item = append_key_down_match(
        &mut nodes,
        input_key_down,
        "Enter",
        non_empty_text,
        skip,
        key.0,
        alloc().0,
    );
    let cleared_input = alloc();
    append_then(&mut nodes, appended_item, empty, cleared_input.0);
    let latest_input = alloc();
    let input_hold = alloc();
    append_latest_hold(
        &mut nodes,
        empty,
        vec![input_change, cleared_input],
        latest_input.0,
        input_hold.0,
        LatestHoldMode::AlwaysCreateLatest,
    );
    append_value_sink(&mut nodes, input_hold, config.input_sink, alloc().0);

    let initial_items = append_literal(
        &mut nodes,
        KernelValue::List(
            config
                .initial_items
                .iter()
                .map(|item| KernelValue::from(*item))
                .collect(),
        ),
        alloc().0,
    );
    let items_block = alloc();
    let append_to_list = alloc();
    let list_update_inputs = match config.clear_press_port {
        Some(clear_press_port) => {
            let clear_body = append_source_port_triggered_updates(
                &mut nodes,
                &[SourcePortTriggeredBodyConfig {
                    source_port: clear_press_port,
                    body: initial_items,
                    source_node_id: alloc().0,
                    then_node_id: alloc().0,
                }],
                alloc().0,
                LatestHoldMode::OnlyWhenMultiple,
            );
            vec![append_to_list, clear_body]
        }
        None => vec![append_to_list],
    };
    let list_updates = alloc();
    let items_hold = alloc();
    let additional_update_inputs = list_update_inputs.into_iter().skip(1).collect();
    append_list_state_hold(
        &mut nodes,
        items_block,
        initial_items,
        appended_item,
        append_to_list.0,
        additional_update_inputs,
        list_updates.0,
        items_hold.0,
    );
    append_value_sink(&mut nodes, items_hold, config.items_list_sink, alloc().0);
    nodes.push(IrNode {
        id: items_block,
        source_expr: None,
        kind: IrNodeKind::Block {
            inputs: vec![items_hold],
        },
    });

    let count = alloc();
    let prefix = (!config.count_prefix.is_empty()).then(|| (config.count_prefix, alloc().0));
    let suffix = config.count_suffix.map(|suffix| (suffix, alloc().0));
    append_decorated_list_count_sink(
        &mut nodes,
        items_hold,
        count.0,
        prefix,
        suffix,
        alloc().0,
        config.count_sink,
        alloc().0,
    );
    append_append_list_derived_count_sinks(&mut nodes, items_hold, &mut alloc, config);

    AppendListRuntime { nodes }
}

fn append_append_list_derived_count_sinks(
    nodes: &mut Vec<IrNode>,
    items_hold: NodeId,
    alloc: &mut impl FnMut() -> NodeId,
    config: &AppendListRuntimeConfig<'_>,
) {
    for derived in config.derived_count_sinks {
        let derived_list = append_append_list_derived_list(nodes, items_hold, derived.list, alloc);
        let prefix = (!derived.prefix.is_empty()).then(|| (derived.prefix, alloc().0));
        let suffix = derived.suffix.map(|suffix| (suffix, alloc().0));
        append_decorated_list_count_sink(
            nodes,
            derived_list,
            alloc().0,
            prefix,
            suffix,
            alloc().0,
            derived.sink,
            alloc().0,
        );
    }
}

fn append_append_list_derived_list(
    nodes: &mut Vec<IrNode>,
    items_hold: NodeId,
    spec: AppendListDerivedListSpec,
    alloc: &mut impl FnMut() -> NodeId,
) -> NodeId {
    match spec {
        AppendListDerivedListSpec::RetainLiteralBool(value) => {
            let predicate = append_literal(nodes, KernelValue::from(value), alloc().0);
            let retained_list = alloc();
            nodes.push(IrNode {
                id: retained_list,
                source_expr: None,
                kind: IrNodeKind::ListRetain {
                    list: items_hold,
                    predicate,
                },
            });
            retained_list
        }
    }
}

define_source_parsed_entrypoint!(
    try_lower_list_map_block,
    try_lower_list_map_block_from_expressions,
    ListMapBlockProgram
);

fn try_lower_list_map_block_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ListMapBlockProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &LIST_HOST_ONLY_SURFACE_GROUP,
        "dual_mapped_label_stripes_document",
        |program| match program {
            LoweredProgram::ListMapBlock(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_list_retain_count,
    try_lower_list_retain_count_from_expressions,
    ListRetainCountProgram
);

fn try_lower_list_retain_count_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ListRetainCountProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &LIST_APPEND_LIST_SURFACE_GROUP,
        "counted_filtered_append_list_document",
        |program| match program {
            LoweredProgram::ListRetainCount(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_list_object_state,
    try_lower_list_object_state_from_expressions,
    ListObjectStateProgram
);

fn try_lower_list_object_state_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ListObjectStateProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &LIST_HOST_ONLY_SURFACE_GROUP,
        "independent_object_counters_document",
        |program| match program {
            LoweredProgram::ListObjectState(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_list_retain_remove,
    try_lower_list_retain_remove_from_expressions,
    ListRetainRemoveProgram
);

fn try_lower_list_retain_remove_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ListRetainRemoveProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &LIST_APPEND_LIST_SURFACE_GROUP,
        "removable_append_list_document",
        |program| match program {
            LoweredProgram::ListRetainRemove(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_shopping_list,
    try_lower_shopping_list_from_expressions,
    ShoppingListProgram
);

fn try_lower_shopping_list_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ShoppingListProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &LIST_APPEND_LIST_SURFACE_GROUP,
        "clearable_append_list_document",
        |program| match program {
            LoweredProgram::ShoppingList(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_filter_checkbox_bug,
    try_lower_filter_checkbox_bug_from_expressions,
    FilterCheckboxBugProgram
);

fn try_lower_filter_checkbox_bug_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<FilterCheckboxBugProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &CHECKBOX_LIST_SURFACE_GROUP,
        "filterable_checkbox_list_document",
        |program| match program {
            LoweredProgram::FilterCheckboxBug(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_checkbox_test,
    try_lower_checkbox_test_from_expressions,
    CheckboxTestProgram
);

fn try_lower_checkbox_test_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<CheckboxTestProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &CHECKBOX_LIST_SURFACE_GROUP,
        "independent_checkbox_list_document",
        |program| match program {
            LoweredProgram::CheckboxTest(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_chained_list_remove_bug,
    try_lower_chained_list_remove_bug_from_expressions,
    ChainedListRemoveBugProgram
);

fn try_lower_chained_list_remove_bug_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ChainedListRemoveBugProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &CHECKBOX_LIST_SURFACE_GROUP,
        "removable_checkbox_list_document",
        |program| match program {
            LoweredProgram::ChainedListRemoveBug(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(try_lower_crud, try_lower_crud_from_expressions, CrudProgram);

fn try_lower_crud_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<CrudProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &LIST_TITLED_COLUMN_SURFACE_GROUP,
        "selectable_record_column_document",
        |program| match program {
            LoweredProgram::Crud(program) => Some(program),
            _ => None,
        },
    )
}

const FORM_RUNTIME_SURFACE_GROUP: SurfaceProgramGroup<
    FormRuntimeSurfaceConfig<'static>,
    LoweredProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_form_runtime_surface_owned,
    cases: &[
        SurfaceProgramCase {
            source: FormRuntimeSurfaceConfig {
                config: FormRuntimeProgramConfig {
                    validation: StructuralValidationSpec {
                        subset: "bidirectional_conversion_form_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[],
                        hold_paths: &[
                            ["store", "celsius_raw"].as_slice(),
                            ["store", "fahrenheit_raw"].as_slice(),
                            ["store", "last_edited"].as_slice(),
                        ],
                        required_functions: &["root_element", "converter_row"],
                        alias_paths: &[
                            ["elements", "celsius_input", "event", "change", "text"].as_slice(),
                            ["elements", "fahrenheit_input", "event", "change", "text"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "text_input"].as_slice(),
                            ["Text", "to_number"].as_slice(),
                            ["Math", "round"].as_slice(),
                        ],
                        text_fragments: &["Temperature Converter", "Celsius", "Fahrenheit", "="],
                        require_hold: true,
                        require_latest: true,
                        require_then: true,
                        require_when: false,
                        require_while: true,
                    },
                    sink_bindings: &[
                        ("celsius_text", SinkPortId(1801)),
                        ("fahrenheit_text", SinkPortId(1802)),
                    ],
                    source_bindings: &[
                        ("store.elements.celsius_input", SourcePortId(1800)),
                        ("store.elements.fahrenheit_input", SourcePortId(1802)),
                    ],
                    view_site: ViewSiteId(1800),
                    function_instance: FunctionInstanceId(1800),
                    timer_source_children: &[],
                },
                build_ir: build_bidirectional_conversion_ir,
                program: FormRuntimeLoweredProgramSpec::TemperatureConverter,
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: FormRuntimeSurfaceConfig {
                config: FormRuntimeProgramConfig {
                    validation: StructuralValidationSpec {
                        subset: "selectable_dual_date_form_document",
                        top_level_bindings: &["store", "document"],
                        required_paths: &[],
                        hold_paths: &[
                            ["store", "flight_type"].as_slice(),
                            ["store", "departure_date"].as_slice(),
                            ["store", "return_date"].as_slice(),
                            ["store", "booked"].as_slice(),
                        ],
                        required_functions: &["root_element"],
                        alias_paths: &[
                            ["elements", "flight_select", "event", "change", "value"].as_slice(),
                            ["elements", "departure_input", "event", "change", "text"].as_slice(),
                            ["elements", "return_input", "event", "change", "text"].as_slice(),
                            ["elements", "book_button", "event", "press"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "select"].as_slice(),
                            ["Element", "text_input"].as_slice(),
                            ["Element", "button"].as_slice(),
                        ],
                        text_fragments: &[
                            "Flight Booker",
                            "One-way flight",
                            "Return flight",
                            "Book",
                            "Booked one-way flight on ",
                            "Booked return flight: ",
                        ],
                        require_hold: true,
                        require_latest: false,
                        require_then: true,
                        require_when: true,
                        require_while: true,
                    },
                    sink_bindings: &[
                        ("store.booked", SinkPortId(1906)),
                        ("store.elements.flight_select.selected", SinkPortId(1901)),
                        ("store.departure_date", SinkPortId(1902)),
                        ("store.return_date", SinkPortId(1903)),
                        ("store.elements.return_input.disabled", SinkPortId(1904)),
                        ("store.elements.book_button.disabled", SinkPortId(1905)),
                    ],
                    source_bindings: &[
                        ("store.elements.flight_select", SourcePortId(1900)),
                        ("store.elements.departure_input", SourcePortId(1901)),
                        ("store.elements.return_input", SourcePortId(1902)),
                        ("store.elements.book_button", SourcePortId(1903)),
                    ],
                    view_site: ViewSiteId(1900),
                    function_instance: FunctionInstanceId(1900),
                    timer_source_children: &[],
                },
                build_ir: build_selectable_dual_date_ir,
                program: FormRuntimeLoweredProgramSpec::FlightBooker,
            },
            wrap: wrap_lowered_program,
        },
        SurfaceProgramCase {
            source: FormRuntimeSurfaceConfig {
                config: FormRuntimeProgramConfig {
                    validation: StructuralValidationSpec {
                        subset: "resettable_timed_progress_document",
                        top_level_bindings: &["tick", "store", "document"],
                        required_paths: &[],
                        hold_paths: &[
                            ["store", "max_duration"].as_slice(),
                            ["store", "raw_elapsed"].as_slice(),
                        ],
                        required_functions: &[
                            "root_element",
                            "gauge_row",
                            "elapsed_row",
                            "duration_row",
                        ],
                        alias_paths: &[
                            ["elements", "duration_slider", "event", "change", "value"].as_slice(),
                            ["elements", "reset_button", "event", "press"].as_slice(),
                        ],
                        function_call_paths: &[
                            ["Timer", "interval"].as_slice(),
                            ["Document", "new"].as_slice(),
                            ["Element", "stripe"].as_slice(),
                            ["Element", "label"].as_slice(),
                            ["Element", "slider"].as_slice(),
                            ["Element", "button"].as_slice(),
                            ["Math", "min"].as_slice(),
                            ["Math", "round"].as_slice(),
                        ],
                        text_fragments: &["Timer", "Duration:", "Elapsed Time:", "Reset"],
                        require_hold: true,
                        require_latest: true,
                        require_then: true,
                        require_when: false,
                        require_while: false,
                    },
                    sink_bindings: &[
                        ("elapsed_display", SinkPortId(1953)),
                        ("max_duration", SinkPortId(1955)),
                        ("progress_percent", SinkPortId(1952)),
                    ],
                    source_bindings: &[
                        ("store.elements.duration_slider", SourcePortId(1950)),
                        ("store.elements.reset_button", SourcePortId(1951)),
                    ],
                    view_site: ViewSiteId(1950),
                    function_instance: FunctionInstanceId(1950),
                    timer_source_children: &[TimerSourceChildConfig {
                        port: SourcePortId(1952),
                        interval_ms: 100,
                    }],
                },
                build_ir: build_resettable_timed_progress_ir,
                program: FormRuntimeLoweredProgramSpec::Timer,
            },
            wrap: wrap_lowered_program,
        },
    ],
};

define_source_parsed_entrypoint!(
    try_lower_temperature_converter,
    try_lower_temperature_converter_from_expressions,
    TemperatureConverterProgram
);

fn try_lower_temperature_converter_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<TemperatureConverterProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &FORM_RUNTIME_SURFACE_GROUP,
        "bidirectional_conversion_form_document",
        |program| match program {
            LoweredProgram::TemperatureConverter(program) => Some(program),
            _ => None,
        },
    )
}

fn build_bidirectional_conversion_ir() -> IrProgram {
    let empty_text = NodeId(1800);
    let mut ir = Vec::new();
    append_literal(&mut ir, KernelValue::from(""), empty_text.0);
    append_literal(&mut ir, KernelValue::Tag("None".to_string()), 1805);
    append_literal(&mut ir, KernelValue::Tag("Celsius".to_string()), 1806);
    let mut temperature_inputs = append_source_backed_holds(
        &mut ir,
        &[
            SourceBackedHoldConfig {
                seed: empty_text,
                source_port: SourcePortId(1800),
                source_node_id: 1801,
                hold_node_id: 1803,
            },
            SourceBackedHoldConfig {
                seed: empty_text,
                source_port: SourcePortId(1802),
                source_node_id: 1802,
                hold_node_id: 1804,
            },
        ],
    )
    .into_iter();
    let celsius_raw = temperature_inputs
        .next()
        .expect("temperature_converter should define a celsius input hold");
    let fahrenheit_raw = temperature_inputs
        .next()
        .expect("temperature_converter should define a fahrenheit input hold");
    append_literal(&mut ir, KernelValue::Tag("Fahrenheit".to_string()), 1808);
    append_literal(&mut ir, KernelValue::Tag("NaN".to_string()), 1813);
    append_literal(&mut ir, KernelValue::from(32.0), 1814);
    append_literal(&mut ir, KernelValue::from(5.0), 1815);
    append_literal(&mut ir, KernelValue::from(9.0), 1816);
    ir.extend([
        IrNode {
            id: NodeId(1812),
            source_expr: None,
            kind: IrNodeKind::TextToNumber {
                input: fahrenheit_raw.hold,
            },
        },
        IrNode {
            id: NodeId(1817),
            source_expr: None,
            kind: IrNodeKind::Sub {
                lhs: NodeId(1812),
                rhs: NodeId(1814),
            },
        },
        IrNode {
            id: NodeId(1818),
            source_expr: None,
            kind: IrNodeKind::Mul {
                lhs: NodeId(1817),
                rhs: NodeId(1815),
            },
        },
        IrNode {
            id: NodeId(1819),
            source_expr: None,
            kind: IrNodeKind::Div {
                lhs: NodeId(1818),
                rhs: NodeId(1816),
            },
        },
        IrNode {
            id: NodeId(1820),
            source_expr: None,
            kind: IrNodeKind::MathRound {
                input: NodeId(1819),
            },
        },
        IrNode {
            id: NodeId(1823),
            source_expr: None,
            kind: IrNodeKind::TextToNumber {
                input: celsius_raw.hold,
            },
        },
        IrNode {
            id: NodeId(1824),
            source_expr: None,
            kind: IrNodeKind::Mul {
                lhs: NodeId(1823),
                rhs: NodeId(1816),
            },
        },
        IrNode {
            id: NodeId(1825),
            source_expr: None,
            kind: IrNodeKind::Div {
                lhs: NodeId(1824),
                rhs: NodeId(1815),
            },
        },
        IrNode {
            id: NodeId(1826),
            source_expr: None,
            kind: IrNodeKind::Add {
                lhs: NodeId(1825),
                rhs: NodeId(1814),
            },
        },
        IrNode {
            id: NodeId(1827),
            source_expr: None,
            kind: IrNodeKind::MathRound {
                input: NodeId(1826),
            },
        },
    ]);
    append_while(
        &mut ir,
        NodeId(1812),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::Tag("NaN".to_string()),
            result: NodeId(1800),
        }],
        NodeId(1820),
        1821,
    );
    append_while(
        &mut ir,
        NodeId(1811),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::Tag("Fahrenheit".to_string()),
            result: NodeId(1821),
        }],
        celsius_raw.hold,
        1822,
    );
    append_while(
        &mut ir,
        NodeId(1823),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::Tag("NaN".to_string()),
            result: NodeId(1800),
        }],
        NodeId(1827),
        1828,
    );
    append_while(
        &mut ir,
        NodeId(1811),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::Tag("Celsius".to_string()),
            result: NodeId(1828),
        }],
        fahrenheit_raw.hold,
        1829,
    );
    append_literal_sink(
        &mut ir,
        KernelValue::from("Temperature Converter"),
        1830,
        SinkPortId(1800),
        1834,
    );
    append_value_sink(&mut ir, NodeId(1822), SinkPortId(1801), 1835);
    append_value_sink(&mut ir, NodeId(1829), SinkPortId(1802), 1836);
    append_literal_sink(
        &mut ir,
        KernelValue::from("Celsius"),
        1831,
        SinkPortId(1803),
        1837,
    );
    append_literal_sink(
        &mut ir,
        KernelValue::from("="),
        1832,
        SinkPortId(1804),
        1838,
    );
    append_literal_sink(
        &mut ir,
        KernelValue::from("Fahrenheit"),
        1833,
        SinkPortId(1805),
        1839,
    );
    append_source_triggered_hold(
        &mut ir,
        NodeId(1805),
        &[
            SourceTriggeredBodyConfig {
                source: celsius_raw.source,
                body: NodeId(1806),
                then: NodeId(1807),
            },
            SourceTriggeredBodyConfig {
                source: fahrenheit_raw.source,
                body: NodeId(1808),
                then: NodeId(1809),
            },
        ],
        1810,
        1811,
        LatestHoldMode::AlwaysCreateLatest,
    );

    ir.into()
}

define_source_parsed_entrypoint!(
    try_lower_flight_booker,
    try_lower_flight_booker_from_expressions,
    FlightBookerProgram
);

fn try_lower_flight_booker_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<FlightBookerProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &FORM_RUNTIME_SURFACE_GROUP,
        "selectable_dual_date_form_document",
        |program| match program {
            LoweredProgram::FlightBooker(program) => Some(program),
            _ => None,
        },
    )
}

fn build_selectable_dual_date_ir() -> IrProgram {
    let one_way = NodeId(1900);
    let default_date = NodeId(1903);
    let mut ir = Vec::new();
    append_literal(&mut ir, KernelValue::from("one-way"), one_way.0);
    append_literal(&mut ir, KernelValue::from("2026-03-03"), default_date.0);
    append_literal(&mut ir, KernelValue::from(true), 1908);
    append_literal(&mut ir, KernelValue::from(false), 1909);
    append_literal(&mut ir, KernelValue::from("return"), 1910);
    let mut flight_inputs = append_source_backed_holds(
        &mut ir,
        &[
            SourceBackedHoldConfig {
                seed: one_way,
                source_port: SourcePortId(1900),
                source_node_id: 1901,
                hold_node_id: 1902,
            },
            SourceBackedHoldConfig {
                seed: default_date,
                source_port: SourcePortId(1901),
                source_node_id: 1904,
                hold_node_id: 1905,
            },
            SourceBackedHoldConfig {
                seed: default_date,
                source_port: SourcePortId(1902),
                source_node_id: 1906,
                hold_node_id: 1907,
            },
        ],
    )
    .into_iter();
    let flight_type = flight_inputs
        .next()
        .expect("flight_booker should define a flight-type input hold");
    let departure_date = flight_inputs
        .next()
        .expect("flight_booker should define a departure-date input hold");
    let return_date = flight_inputs
        .next()
        .expect("flight_booker should define a return-date input hold");
    append_literal(
        &mut ir,
        KernelValue::from("Booked one-way flight on "),
        1916,
    );
    append_literal(&mut ir, KernelValue::from("Booked return flight: "), 1918);
    append_literal(&mut ir, KernelValue::from(" to "), 1919);
    ir.extend([IrNode {
        id: NodeId(1912),
        source_expr: None,
        kind: IrNodeKind::Ge {
            lhs: return_date.hold,
            rhs: departure_date.hold,
        },
    }]);
    append_while(
        &mut ir,
        flight_type.hold,
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from("return"),
            result: NodeId(1908),
        }],
        NodeId(1909),
        1911,
    );
    append_while(
        &mut ir,
        NodeId(1911),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(false),
            result: NodeId(1908),
        }],
        NodeId(1912),
        1913,
    );
    append_text_join(&mut ir, vec![NodeId(1916), departure_date.hold], 1917);
    append_text_join(
        &mut ir,
        vec![
            NodeId(1918),
            departure_date.hold,
            NodeId(1919),
            return_date.hold,
        ],
        1920,
    );
    append_when(
        &mut ir,
        NodeId(1911),
        vec![
            crate::ir::MatchArm {
                matcher: KernelValue::from(false),
                result: NodeId(1917),
            },
            crate::ir::MatchArm {
                matcher: KernelValue::from(true),
                result: NodeId(1920),
            },
        ],
        NodeId(1917),
        1921,
    );
    append_literal(&mut ir, KernelValue::from(""), 1923);
    ir.extend([IrNode {
        id: NodeId(1940),
        source_expr: None,
        kind: IrNodeKind::Skip,
    }]);
    append_while(
        &mut ir,
        NodeId(1915),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(1921),
        }],
        NodeId(1940),
        1922,
    );
    append_while(
        &mut ir,
        NodeId(1911),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(1909),
        }],
        NodeId(1908),
        1925,
    );
    append_while(
        &mut ir,
        NodeId(1913),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(1909),
        }],
        NodeId(1908),
        1926,
    );
    append_source_port_triggered_updates(
        &mut ir,
        &[SourcePortTriggeredBodyConfig {
            source_port: SourcePortId(1903),
            body: NodeId(1913),
            source_node_id: 1914,
            then_node_id: 1915,
        }],
        1915,
        LatestHoldMode::OnlyWhenMultiple,
    );
    append_seeded_hold(&mut ir, NodeId(1923), NodeId(1922), 1924);
    append_literal_sink(
        &mut ir,
        KernelValue::from("Flight Booker"),
        1927,
        SinkPortId(1900),
        1930,
    );
    append_value_sink(&mut ir, flight_type.hold, SinkPortId(1901), 1931);
    append_value_sink(&mut ir, departure_date.hold, SinkPortId(1902), 1932);
    append_value_sink(&mut ir, return_date.hold, SinkPortId(1903), 1933);
    append_value_sink(&mut ir, NodeId(1925), SinkPortId(1904), 1934);
    append_value_sink(&mut ir, NodeId(1926), SinkPortId(1905), 1935);
    append_value_sink(&mut ir, NodeId(1924), SinkPortId(1906), 1936);

    ir.into()
}

define_source_parsed_entrypoint!(
    try_lower_timer,
    try_lower_timer_from_expressions,
    TimerProgram
);

fn try_lower_timer_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<TimerProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &FORM_RUNTIME_SURFACE_GROUP,
        "resettable_timed_progress_document",
        |program| match program {
            LoweredProgram::Timer(program) => Some(program),
            _ => None,
        },
    )
}

fn build_resettable_timed_progress_ir() -> IrProgram {
    let mut ir = Vec::new();
    append_literal(&mut ir, KernelValue::from(15.0), 1950);
    let duration_slider = append_source_port(&mut ir, SourcePortId(1950), 1951);
    append_literal(&mut ir, KernelValue::from(1.0), 1953);
    append_literal(&mut ir, KernelValue::from(0.0), 1957);
    append_literal(&mut ir, KernelValue::from(0.1), 1959);
    append_literal(&mut ir, KernelValue::from(10.0), 1967);
    append_literal(&mut ir, KernelValue::from(100.0), 1972);
    append_literal(&mut ir, KernelValue::from("%"), 1978);
    append_literal(&mut ir, KernelValue::from("Duration:"), 1980);
    append_literal(&mut ir, KernelValue::from("s"), 1981);
    ir.extend([
        IrNode {
            id: NodeId(1952),
            source_expr: None,
            kind: IrNodeKind::TextToNumber {
                input: duration_slider,
            },
        },
        IrNode {
            id: NodeId(1954),
            source_expr: None,
            kind: IrNodeKind::Ge {
                lhs: NodeId(1952),
                rhs: NodeId(1953),
            },
        },
        IrNode {
            id: NodeId(1960),
            source_expr: None,
            kind: IrNodeKind::Add {
                lhs: NodeId(1965),
                rhs: NodeId(1959),
            },
        },
        IrNode {
            id: NodeId(1966),
            source_expr: None,
            kind: IrNodeKind::MathMin {
                lhs: NodeId(1965),
                rhs: NodeId(1956),
            },
        },
        IrNode {
            id: NodeId(1968),
            source_expr: None,
            kind: IrNodeKind::Mul {
                lhs: NodeId(1966),
                rhs: NodeId(1967),
            },
        },
        IrNode {
            id: NodeId(1969),
            source_expr: None,
            kind: IrNodeKind::MathRound {
                input: NodeId(1968),
            },
        },
        IrNode {
            id: NodeId(1970),
            source_expr: None,
            kind: IrNodeKind::Div {
                lhs: NodeId(1969),
                rhs: NodeId(1967),
            },
        },
        IrNode {
            id: NodeId(1971),
            source_expr: None,
            kind: IrNodeKind::Div {
                lhs: NodeId(1966),
                rhs: NodeId(1956),
            },
        },
        IrNode {
            id: NodeId(1973),
            source_expr: None,
            kind: IrNodeKind::Mul {
                lhs: NodeId(1971),
                rhs: NodeId(1972),
            },
        },
        IrNode {
            id: NodeId(1974),
            source_expr: None,
            kind: IrNodeKind::MathMin {
                lhs: NodeId(1973),
                rhs: NodeId(1972),
            },
        },
        IrNode {
            id: NodeId(1975),
            source_expr: None,
            kind: IrNodeKind::MathRound {
                input: NodeId(1974),
            },
        },
    ]);
    append_while(
        &mut ir,
        NodeId(1954),
        vec![crate::ir::MatchArm {
            matcher: KernelValue::from(true),
            result: NodeId(1952),
        }],
        NodeId(1950),
        1955,
    );
    append_literal_sink(
        &mut ir,
        KernelValue::from("Timer"),
        1976,
        SinkPortId(1950),
        1984,
    );
    append_literal_sink(
        &mut ir,
        KernelValue::from("Elapsed Time:"),
        1977,
        SinkPortId(1951),
        1985,
    );
    append_value_sink(&mut ir, NodeId(1975), SinkPortId(1952), 1986);
    append_value_sink(&mut ir, NodeId(1970), SinkPortId(1953), 1989);
    append_text_join_sink(
        &mut ir,
        vec![NodeId(1956), NodeId(1981)],
        1983,
        SinkPortId(1956),
        1990,
    );
    append_seeded_hold(&mut ir, NodeId(1950), NodeId(1955), 1956);
    append_value_sink(&mut ir, NodeId(1980), SinkPortId(1954), 1987);
    append_value_sink(&mut ir, NodeId(1956), SinkPortId(1955), 1988);
    append_source_port_triggered_hold(
        &mut ir,
        NodeId(1957),
        &[
            SourcePortTriggeredBodyConfig {
                source_port: SourcePortId(1952),
                body: NodeId(1960),
                source_node_id: 1958,
                then_node_id: 1961,
            },
            SourcePortTriggeredBodyConfig {
                source_port: SourcePortId(1951),
                body: NodeId(1957),
                source_node_id: 1962,
                then_node_id: 1963,
            },
        ],
        1964,
        1965,
        LatestHoldMode::AlwaysCreateLatest,
    );

    ir.into()
}

define_source_parsed_entrypoint!(
    try_lower_interval,
    try_lower_interval_from_expressions,
    IntervalProgram
);

fn try_lower_interval_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<IntervalProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &INTERVAL_SURFACE_GROUP,
        "summed_interval_signal_document",
        |program| match program {
            LoweredProgram::Interval(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_interval_hold,
    try_lower_interval_hold_from_expressions,
    IntervalProgram
);

fn try_lower_interval_hold_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<IntervalProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &INTERVAL_SURFACE_GROUP,
        "held_interval_signal_document",
        |program| match program {
            LoweredProgram::IntervalHold(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_fibonacci,
    try_lower_fibonacci_from_expressions,
    FibonacciProgram
);

fn try_lower_fibonacci_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<FibonacciProgram, String> {
    lower_bindings_group_typed_program(
        expressions,
        &DISPLAY_SINGLE_SINK_BINDINGS_GROUP,
        "sequence_message_display",
        |program| match program {
            LoweredProgram::Fibonacci(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_layers,
    try_lower_layers_from_expressions,
    LayersProgram
);

fn try_lower_layers_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<LayersProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &DISPLAY_SINK_VALUE_SURFACE_GROUP,
        "static_stack_display",
        |program| match program {
            LoweredProgram::Layers(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_pages,
    try_lower_pages_from_expressions,
    PagesProgram
);

fn try_lower_pages_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<PagesProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_SURFACE_GROUP,
        "nav_selection_document",
        |program| match program {
            LoweredProgram::Pages(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_latest,
    try_lower_latest_from_expressions,
    LatestProgram
);

fn try_lower_latest_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<LatestProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_SURFACE_GROUP,
        "latest_signal_document",
        |program| match program {
            LoweredProgram::Latest(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_text_interpolation_update,
    try_lower_text_interpolation_update_from_expressions,
    TextInterpolationUpdateProgram
);

fn try_lower_text_interpolation_update_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<TextInterpolationUpdateProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_SURFACE_GROUP,
        "toggle_templated_label_document",
        |program| match program {
            LoweredProgram::TextInterpolationUpdate(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(try_lower_then, try_lower_then_from_expressions, ThenProgram);

fn try_lower_then_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ThenProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &TIMED_FLOW_SURFACE_GROUP,
        "timed_addition_hold_document",
        |program| match program {
            LoweredProgram::Then(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(try_lower_when, try_lower_when_from_expressions, WhenProgram);

fn try_lower_when_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<WhenProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &TIMED_FLOW_SURFACE_GROUP,
        "timed_operation_hold_document",
        |program| match program {
            LoweredProgram::When(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_while,
    try_lower_while_from_expressions,
    WhileProgram
);

fn try_lower_while_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<WhileProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &TIMED_FLOW_SURFACE_GROUP,
        "timed_operation_stream_document",
        |program| match program {
            LoweredProgram::While(program) => Some(program),
            _ => None,
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SumOfStepsSpec {
    step: i64,
    interval_ms: u32,
}

#[derive(Clone, Copy)]
enum TimedCounterOutputMode {
    SummedUpdates,
    HoldOnTick,
}

#[derive(Clone, Copy)]
struct IntervalSignalSurfaceConfig<'a> {
    subset: &'static str,
    surface: TimerBackedSignalSurfaceConfig<'a>,
    derive_interval_ms:
        for<'b> fn(&BTreeMap<String, &'b StaticSpannedExpression>) -> Result<u32, String>,
    base_node_id: u32,
    output_mode: TimedCounterOutputMode,
}

impl LoweringSubset for IntervalSignalSurfaceConfig<'_> {
    fn lowering_subset(&self) -> &'static str {
        self.subset
    }
}

const INTERVAL_SURFACE_GROUP: SurfaceProgramGroup<
    IntervalSignalSurfaceConfig<'static>,
    IntervalProgram,
> = SurfaceProgramGroup {
    lower_surface: lower_interval_signal_surface_owned,
    cases: &[
        SurfaceProgramCase {
            source: IntervalSignalSurfaceConfig {
                subset: "summed_interval_signal_document",
                surface: TimerBackedSignalSurfaceConfig {
                    sink_bindings: &[("document", SinkPortId(1980))],
                    press_bindings: &[],
                    root_view_site: ViewSiteId(1980),
                    function_instance: FunctionInstanceId(1980),
                    root_binding_name: Some("document"),
                    value_sink: SinkPortId(1980),
                    tick_port: SourcePortId(1980),
                },
                derive_interval_ms: derive_interval_signal_interval_ms,
                base_node_id: 1980,
                output_mode: TimedCounterOutputMode::SummedUpdates,
            },
            wrap: LoweredProgram::Interval,
        },
        SurfaceProgramCase {
            source: IntervalSignalSurfaceConfig {
                subset: "held_interval_signal_document",
                surface: TimerBackedSignalSurfaceConfig {
                    sink_bindings: &[("counter", SinkPortId(1981))],
                    press_bindings: &[],
                    root_view_site: ViewSiteId(1981),
                    function_instance: FunctionInstanceId(1981),
                    root_binding_name: None,
                    value_sink: SinkPortId(1981),
                    tick_port: SourcePortId(1981),
                },
                derive_interval_ms: derive_interval_hold_signal_interval_ms,
                base_node_id: 1985,
                output_mode: TimedCounterOutputMode::HoldOnTick,
            },
            wrap: LoweredProgram::IntervalHold,
        },
    ],
};

fn lower_interval_signal_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: IntervalSignalSurfaceConfig<'static>,
) -> Result<IntervalProgram, String> {
    lower_timer_backed_signal_surface(
        expressions,
        &config.surface,
        |bindings, tick_port, value_sink| {
            Ok((
                (config.derive_interval_ms)(bindings)?,
                lower_timed_counter_signal_ir(
                    SumOfStepsSpec {
                        step: 1,
                        interval_ms: 0,
                    },
                    tick_port,
                    value_sink,
                    config.base_node_id,
                    config.output_mode,
                )
                .into(),
            ))
        },
    )
}

fn derive_interval_signal_interval_ms(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<u32, String> {
    let document =
        require_top_level_binding_expr(bindings, "summed_interval_signal_document", "document")?;
    lower_interval_document_expression(document)
}

fn derive_interval_hold_signal_interval_ms(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<u32, String> {
    let tick = require_top_level_binding_expr(bindings, "held_interval_signal_document", "tick")?;
    let counter =
        require_top_level_binding_expr(bindings, "held_interval_signal_document", "counter")?;
    let document =
        require_top_level_binding_expr(bindings, "held_interval_signal_document", "document")?;

    let interval_ms = extract_timer_interval_ms(tick)?;
    lower_interval_hold_counter(counter)?;
    lower_interval_hold_document(document)?;
    Ok(interval_ms)
}

struct TimedFlowLoweringBase<'a> {
    bindings: BTreeMap<String, &'a StaticSpannedExpression>,
    input_a: SumOfStepsSpec,
    input_b: SumOfStepsSpec,
    host_view: HostViewIr,
}

#[derive(Clone, Copy)]
struct TimedFlowProgramConfig<'a> {
    validation: StructuralValidationSpec<'a>,
    result_binding: &'a str,
    addition_button_binding: &'a str,
    subtraction_button_binding: Option<&'a str>,
    input_a_persistence: Option<LoweringPathPersistenceSeed<'a>>,
    input_b_persistence: Option<LoweringPathPersistenceSeed<'a>>,
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    input_a_tick_port: SourcePortId,
    input_b_tick_port: SourcePortId,
    addition_press_port: SourcePortId,
    subtraction_press_port: Option<SourcePortId>,
    input_a_sink: SinkPortId,
    input_b_sink: SinkPortId,
    result_sink: SinkPortId,
}

#[derive(Clone, Copy)]
struct TimedFlowSurfaceConfig {
    config: TimedFlowProgramConfig<'static>,
    ir_mode: TimedFlowIrMode,
    program: TimedFlowLoweredProgramSpec,
}

impl LoweringSubset for TimedFlowSurfaceConfig {
    fn lowering_subset(&self) -> &'static str {
        self.config.validation.lowering_subset()
    }
}

#[derive(Clone, Copy)]
enum TimedFlowIrMode {
    HoldBacked(HoldBackedTimedMathMode),
    DirectOperationChoice,
}

#[derive(Clone, Copy)]
enum TimedFlowLoweredProgramSpec {
    Then,
    When,
    While,
}

fn lower_timed_flow_surface_owned(
    expressions: &[StaticSpannedExpression],
    config: TimedFlowSurfaceConfig,
) -> Result<LoweredProgram, String> {
    let lowering = lower_timed_flow_program_from_config(expressions, config)?;
    build_timed_flow_lowered_program(lowering, config.program)
}

const TIMED_FLOW_SURFACE_GROUP: SurfaceProgramGroup<TimedFlowSurfaceConfig, LoweredProgram> =
    SurfaceProgramGroup {
        lower_surface: lower_timed_flow_surface_owned,
        cases: &[
            SurfaceProgramCase {
                source: TimedFlowSurfaceConfig {
                    config: TimedFlowProgramConfig {
                        validation: StructuralValidationSpec {
                            subset: "timed_addition_hold_document",
                            top_level_bindings: &[
                                "current_sum",
                                "input_a",
                                "input_b",
                                "addition_button",
                                "document",
                            ],
                            required_paths: &[],
                            hold_paths: &[],
                            required_functions: &["sum_of_steps", "input_row"],
                            alias_paths: &[],
                            function_call_paths: &[
                                ["Timer", "interval"].as_slice(),
                                ["Element", "button"].as_slice(),
                            ],
                            text_fragments: &["A + B"],
                            require_hold: true,
                            require_latest: false,
                            require_then: true,
                            require_when: false,
                            require_while: false,
                        },
                        result_binding: "current_sum",
                        addition_button_binding: "addition_button",
                        subtraction_button_binding: None,
                        input_a_persistence: Some(LoweringPathPersistenceSeed {
                            path: &["input_a"],
                            local_slot: 0,
                            persist_kind: PersistKind::Hold,
                        }),
                        input_b_persistence: Some(LoweringPathPersistenceSeed {
                            path: &["input_b"],
                            local_slot: 0,
                            persist_kind: PersistKind::Hold,
                        }),
                        view_site: ViewSiteId(2009),
                        function_instance: FunctionInstanceId(2009),
                        input_a_tick_port: SourcePortId(2010),
                        input_b_tick_port: SourcePortId(2011),
                        addition_press_port: SourcePortId(2012),
                        subtraction_press_port: None,
                        input_a_sink: SinkPortId(2010),
                        input_b_sink: SinkPortId(2011),
                        result_sink: SinkPortId(2012),
                    },
                    ir_mode: TimedFlowIrMode::HoldBacked(HoldBackedTimedMathMode::Then),
                    program: TimedFlowLoweredProgramSpec::Then,
                },
                wrap: wrap_lowered_program,
            },
            SurfaceProgramCase {
                source: TimedFlowSurfaceConfig {
                    config: TimedFlowProgramConfig {
                        validation: StructuralValidationSpec {
                            subset: "timed_operation_hold_document",
                            top_level_bindings: &[
                                "current_result",
                                "operation",
                                "input_a",
                                "input_b",
                                "addition_button",
                                "subtraction_button",
                                "document",
                            ],
                            required_paths: &[],
                            hold_paths: &[],
                            required_functions: &["sum_of_steps", "input_row", "operation_button"],
                            alias_paths: &[],
                            function_call_paths: &[
                                ["Timer", "interval"].as_slice(),
                                ["Element", "button"].as_slice(),
                            ],
                            text_fragments: &["A + B", "A - B"],
                            require_hold: false,
                            require_latest: true,
                            require_then: true,
                            require_when: true,
                            require_while: false,
                        },
                        result_binding: "current_result",
                        addition_button_binding: "addition_button",
                        subtraction_button_binding: Some("subtraction_button"),
                        input_a_persistence: Some(LoweringPathPersistenceSeed {
                            path: &["input_a"],
                            local_slot: 0,
                            persist_kind: PersistKind::Hold,
                        }),
                        input_b_persistence: Some(LoweringPathPersistenceSeed {
                            path: &["input_b"],
                            local_slot: 0,
                            persist_kind: PersistKind::Hold,
                        }),
                        view_site: ViewSiteId(2021),
                        function_instance: FunctionInstanceId(2010),
                        input_a_tick_port: SourcePortId(2013),
                        input_b_tick_port: SourcePortId(2014),
                        addition_press_port: SourcePortId(2015),
                        subtraction_press_port: Some(SourcePortId(2016)),
                        input_a_sink: SinkPortId(2013),
                        input_b_sink: SinkPortId(2014),
                        result_sink: SinkPortId(2015),
                    },
                    ir_mode: TimedFlowIrMode::HoldBacked(HoldBackedTimedMathMode::When),
                    program: TimedFlowLoweredProgramSpec::When,
                },
                wrap: wrap_lowered_program,
            },
            SurfaceProgramCase {
                source: TimedFlowSurfaceConfig {
                    config: TimedFlowProgramConfig {
                        validation: StructuralValidationSpec {
                            subset: "timed_operation_stream_document",
                            top_level_bindings: &[
                                "updating_result",
                                "operation",
                                "input_a",
                                "input_b",
                                "addition_button",
                                "subtraction_button",
                                "document",
                            ],
                            required_paths: &[],
                            hold_paths: &[],
                            required_functions: &["sum_of_steps", "input_row", "operation_button"],
                            alias_paths: &[],
                            function_call_paths: &[
                                ["Timer", "interval"].as_slice(),
                                ["Math", "sum"].as_slice(),
                                ["Element", "button"].as_slice(),
                            ],
                            text_fragments: &["A + B", "A - B"],
                            require_hold: false,
                            require_latest: true,
                            require_then: true,
                            require_when: false,
                            require_while: true,
                        },
                        result_binding: "updating_result",
                        addition_button_binding: "addition_button",
                        subtraction_button_binding: Some("subtraction_button"),
                        input_a_persistence: None,
                        input_b_persistence: None,
                        view_site: ViewSiteId(2033),
                        function_instance: FunctionInstanceId(2011),
                        input_a_tick_port: SourcePortId(2016),
                        input_b_tick_port: SourcePortId(2017),
                        addition_press_port: SourcePortId(2019),
                        subtraction_press_port: Some(SourcePortId(2020)),
                        input_a_sink: SinkPortId(2016),
                        input_b_sink: SinkPortId(2017),
                        result_sink: SinkPortId(2018),
                    },
                    ir_mode: TimedFlowIrMode::DirectOperationChoice,
                    program: TimedFlowLoweredProgramSpec::While,
                },
                wrap: wrap_lowered_program,
            },
        ],
    };

struct TimedMathProgramLowering {
    ir: IrProgram,
    host_view: HostViewIr,
    input_a_tick_port: SourcePortId,
    input_b_tick_port: SourcePortId,
    addition_press_port: SourcePortId,
    subtraction_press_port: Option<SourcePortId>,
    input_a_sink: SinkPortId,
    input_b_sink: SinkPortId,
    result_sink: SinkPortId,
}

impl TimedMathProgramLowering {
    fn into_then_program(self) -> ThenProgram {
        ThenProgram {
            ir: self.ir,
            host_view: self.host_view,
            input_a_tick_port: self.input_a_tick_port,
            input_b_tick_port: self.input_b_tick_port,
            addition_press_port: self.addition_press_port,
            input_a_sink: self.input_a_sink,
            input_b_sink: self.input_b_sink,
            result_sink: self.result_sink,
        }
    }

    fn into_when_program(self) -> Result<WhenProgram, String> {
        let subtraction_press_port = self
            .subtraction_press_port
            .ok_or_else(|| "when subset requires subtraction press port".to_string())?;
        Ok(WhenProgram {
            ir: self.ir,
            host_view: self.host_view,
            input_a_tick_port: self.input_a_tick_port,
            input_b_tick_port: self.input_b_tick_port,
            addition_press_port: self.addition_press_port,
            subtraction_press_port,
            input_a_sink: self.input_a_sink,
            input_b_sink: self.input_b_sink,
            result_sink: self.result_sink,
        })
    }

    fn into_while_program(self) -> Result<WhileProgram, String> {
        let subtraction_press_port = self
            .subtraction_press_port
            .ok_or_else(|| "while subset requires subtraction press port".to_string())?;
        Ok(WhileProgram {
            ir: self.ir,
            host_view: self.host_view,
            input_a_tick_port: self.input_a_tick_port,
            input_b_tick_port: self.input_b_tick_port,
            addition_press_port: self.addition_press_port,
            subtraction_press_port,
            input_a_sink: self.input_a_sink,
            input_b_sink: self.input_b_sink,
            result_sink: self.result_sink,
        })
    }
}

fn build_timed_flow_lowered_program(
    lowering: TimedMathProgramLowering,
    spec: TimedFlowLoweredProgramSpec,
) -> Result<LoweredProgram, String> {
    match spec {
        TimedFlowLoweredProgramSpec::Then => Ok(LoweredProgram::Then(lowering.into_then_program())),
        TimedFlowLoweredProgramSpec::When => lowering.into_when_program().map(LoweredProgram::When),
        TimedFlowLoweredProgramSpec::While => {
            lowering.into_while_program().map(LoweredProgram::While)
        }
    }
}

#[derive(Clone, Copy)]
enum HoldBackedTimedMathMode {
    Then,
    When,
}

#[derive(Clone, Copy)]
enum HoldBackedTimedMathIrMode {
    Then {
        addition_press_port: SourcePortId,
    },
    When {
        addition_press_port: SourcePortId,
        subtraction_press_port: SourcePortId,
    },
}

#[derive(Clone, Copy)]
enum TimedMathActionSourceMode {
    LatestUpdates,
    HeldSelection,
}

#[derive(Clone, Copy)]
enum TimedMathResultMode {
    Hold,
    Direct,
}

struct TimedFlowProgramBase<'a> {
    bindings: BTreeMap<String, &'a StaticSpannedExpression>,
    input_a: SumOfStepsSpec,
    input_b: SumOfStepsSpec,
    host_view: HostViewIr,
    input_a_tick_port: SourcePortId,
    input_b_tick_port: SourcePortId,
    addition_press_port: SourcePortId,
    subtraction_press_port: Option<SourcePortId>,
    input_a_sink: SinkPortId,
    input_b_sink: SinkPortId,
    result_sink: SinkPortId,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimedMathActionKind {
    Add,
    Sub,
}

#[derive(Clone, Copy)]
struct TimedMathActionConfig {
    press_port: SourcePortId,
    kind: TimedMathActionKind,
}

struct TimedMathOperationNodes {
    add: Option<NodeId>,
    sub: Option<NodeId>,
    next_node_id: u32,
}

fn lower_timed_flow_family_base<'a>(
    expressions: &'a [StaticSpannedExpression],
    validation: &StructuralValidationSpec<'_>,
    sink_bindings: &[(&str, SinkPortId)],
    button_bindings: &[(&str, SourcePortId)],
    view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
    input_a_tick_port: SourcePortId,
    input_b_tick_port: SourcePortId,
) -> Result<TimedFlowLoweringBase<'a>, String> {
    lower_bindings_with_generic_host_ir(
        expressions,
        |bindings| {
            let input_a = extract_sum_of_steps_call(
                require_top_level_binding_expr(&bindings, validation.subset, "input_a")?,
                validation.subset,
            )?;
            let input_b = extract_sum_of_steps_call(
                require_top_level_binding_expr(&bindings, validation.subset, "input_b")?,
                validation.subset,
            )?;
            Ok((input_a, input_b))
        },
        |bindings, (input_a, input_b)| {
            lower_generic_host_ir_from_bindings(
                expressions,
                bindings,
                &GenericHostIrSurfaceConfig {
                    validation: validation.clone(),
                    sink_bindings,
                    source_bindings: button_bindings,
                    view_site,
                    function_instance,
                    timer_source_children: &[
                        TimerSourceChildConfig {
                            port: input_a_tick_port,
                            interval_ms: input_a.interval_ms,
                        },
                        TimerSourceChildConfig {
                            port: input_b_tick_port,
                            interval_ms: input_b.interval_ms,
                        },
                    ],
                },
            )
        },
        |bindings, (input_a, input_b), host_view| {
            Ok(TimedFlowLoweringBase {
                bindings,
                input_a,
                input_b,
                host_view,
            })
        },
    )
}

fn lower_timed_flow_program_base<'a>(
    expressions: &'a [StaticSpannedExpression],
    config: &TimedFlowProgramConfig<'_>,
) -> Result<TimedFlowProgramBase<'a>, String> {
    let sink_bindings = vec![
        ("input_a", config.input_a_sink),
        ("input_b", config.input_b_sink),
        (config.result_binding, config.result_sink),
    ];
    let mut button_bindings = vec![(config.addition_button_binding, config.addition_press_port)];
    if let (Some(binding), Some(port)) = (
        config.subtraction_button_binding,
        config.subtraction_press_port,
    ) {
        button_bindings.push((binding, port));
    }

    let base = lower_timed_flow_family_base(
        expressions,
        &config.validation,
        &sink_bindings,
        &button_bindings,
        config.view_site,
        config.function_instance,
        config.input_a_tick_port,
        config.input_b_tick_port,
    )?;

    Ok(TimedFlowProgramBase {
        bindings: base.bindings,
        input_a: base.input_a,
        input_b: base.input_b,
        host_view: base.host_view,
        input_a_tick_port: config.input_a_tick_port,
        input_b_tick_port: config.input_b_tick_port,
        addition_press_port: config.addition_press_port,
        subtraction_press_port: config.subtraction_press_port,
        input_a_sink: config.input_a_sink,
        input_b_sink: config.input_b_sink,
        result_sink: config.result_sink,
    })
}

fn lower_timed_flow_program<'a, T>(
    expressions: &'a [StaticSpannedExpression],
    config: &TimedFlowProgramConfig<'_>,
    build_program: impl FnOnce(TimedFlowProgramBase<'a>) -> Result<T, String>,
) -> Result<T, String> {
    let base = lower_timed_flow_program_base(expressions, config)?;
    build_program(base)
}

fn lower_timed_math_program(
    expressions: &[StaticSpannedExpression],
    config: &TimedFlowProgramConfig<'_>,
    build_ir: impl FnOnce(
        &TimedFlowProgramBase<'_>,
    ) -> Result<(IrProgram, Option<SourcePortId>), String>,
) -> Result<TimedMathProgramLowering, String> {
    lower_timed_flow_program(expressions, config, |base| {
        let (ir, subtraction_press_port) = build_ir(&base)?;
        Ok(TimedMathProgramLowering {
            ir,
            host_view: base.host_view,
            input_a_tick_port: base.input_a_tick_port,
            input_b_tick_port: base.input_b_tick_port,
            addition_press_port: base.addition_press_port,
            subtraction_press_port,
            input_a_sink: base.input_a_sink,
            input_b_sink: base.input_b_sink,
            result_sink: base.result_sink,
        })
    })
}

fn lower_hold_backed_timed_math_program(
    expressions: &[StaticSpannedExpression],
    config: &TimedFlowProgramConfig<'_>,
    mode: HoldBackedTimedMathMode,
) -> Result<TimedMathProgramLowering, String> {
    lower_timed_math_program(expressions, config, |base| match mode {
        HoldBackedTimedMathMode::Then => {
            let input_a_persistence = config.input_a_persistence.ok_or_else(|| {
                format!(
                    "{} subset requires input_a persistence config",
                    config.validation.subset
                )
            })?;
            let input_b_persistence = config.input_b_persistence.ok_or_else(|| {
                format!(
                    "{} subset requires input_b persistence config",
                    config.validation.subset
                )
            })?;
            Ok((
                lower_hold_backed_timed_math_mode_ir(
                    &base.bindings,
                    input_a_persistence,
                    input_b_persistence,
                    base.input_a,
                    base.input_b,
                    base.input_a_tick_port,
                    base.input_b_tick_port,
                    base.input_a_sink,
                    base.input_b_sink,
                    HoldBackedTimedMathIrMode::Then {
                        addition_press_port: base.addition_press_port,
                    },
                    base.result_sink,
                ),
                None,
            ))
        }
        HoldBackedTimedMathMode::When => {
            let input_a_persistence = config.input_a_persistence.ok_or_else(|| {
                format!(
                    "{} subset requires input_a persistence config",
                    config.validation.subset
                )
            })?;
            let input_b_persistence = config.input_b_persistence.ok_or_else(|| {
                format!(
                    "{} subset requires input_b persistence config",
                    config.validation.subset
                )
            })?;
            let subtraction_press_port = base
                .subtraction_press_port
                .ok_or_else(|| "when subset requires subtraction press port".to_string())?;
            Ok((
                lower_hold_backed_timed_math_mode_ir(
                    &base.bindings,
                    input_a_persistence,
                    input_b_persistence,
                    base.input_a,
                    base.input_b,
                    base.input_a_tick_port,
                    base.input_b_tick_port,
                    base.input_a_sink,
                    base.input_b_sink,
                    HoldBackedTimedMathIrMode::When {
                        addition_press_port: base.addition_press_port,
                        subtraction_press_port,
                    },
                    base.result_sink,
                ),
                Some(subtraction_press_port),
            ))
        }
    })
}

fn lower_timed_flow_program_from_config(
    expressions: &[StaticSpannedExpression],
    config: TimedFlowSurfaceConfig,
) -> Result<TimedMathProgramLowering, String> {
    match config.ir_mode {
        TimedFlowIrMode::HoldBacked(mode) => {
            lower_hold_backed_timed_math_program(expressions, &config.config, mode)
        }
        TimedFlowIrMode::DirectOperationChoice => {
            lower_timed_math_program(expressions, &config.config, |base| {
                let subtraction_press_port = base.subtraction_press_port.ok_or_else(|| {
                    format!(
                        "{} subset requires subtraction press port",
                        config.config.validation.subset
                    )
                })?;
                let actions = [
                    TimedMathActionConfig {
                        press_port: base.addition_press_port,
                        kind: TimedMathActionKind::Add,
                    },
                    TimedMathActionConfig {
                        press_port: subtraction_press_port,
                        kind: TimedMathActionKind::Sub,
                    },
                ];
                Ok((
                    lower_timed_math_ir(
                        None,
                        None,
                        None,
                        base.input_a,
                        base.input_b,
                        base.input_a_tick_port,
                        base.input_b_tick_port,
                        base.input_a_sink,
                        base.input_b_sink,
                        &actions,
                        TimedMathActionSourceMode::HeldSelection,
                        TimedMathResultMode::Direct,
                        base.result_sink,
                        2300,
                    ),
                    Some(subtraction_press_port),
                ))
            })
        }
    }
}

fn append_accumulating_timed_hold(
    nodes: &mut Vec<IrNode>,
    spec: SumOfStepsSpec,
    tick_port: SourcePortId,
    base_node_id: u32,
) -> (NodeId, NodeId) {
    let seed = NodeId(base_node_id);
    let source = NodeId(base_node_id + 1);
    let hold = NodeId(base_node_id + 5);

    append_literal(nodes, KernelValue::from(0.0), seed.0);
    append_source_delta_accumulator_hold(
        nodes,
        seed,
        hold.0,
        &[SourceDeltaAccumulatorConfig {
            source_port: tick_port,
            delta: spec.step as f64,
            source_node_id: base_node_id + 1,
            delta_node_id: base_node_id + 2,
            sum_node_id: base_node_id + 3,
            then_node_id: base_node_id + 4,
        }],
        base_node_id + 4,
        LatestHoldMode::OnlyWhenMultiple,
    );

    (source, hold)
}

fn append_accumulating_timed_input(
    nodes: &mut Vec<IrNode>,
    spec: SumOfStepsSpec,
    tick_port: SourcePortId,
    sink_port: SinkPortId,
    base_node_id: u32,
) -> NodeId {
    append_literal(nodes, KernelValue::from(0.0), base_node_id);
    append_source_delta_accumulator_hold_sink(
        nodes,
        NodeId(base_node_id),
        base_node_id + 5,
        &[SourceDeltaAccumulatorConfig {
            source_port: tick_port,
            delta: spec.step as f64,
            source_node_id: base_node_id + 1,
            delta_node_id: base_node_id + 2,
            sum_node_id: base_node_id + 3,
            then_node_id: base_node_id + 4,
        }],
        base_node_id + 4,
        LatestHoldMode::OnlyWhenMultiple,
        sink_port,
        base_node_id + 6,
    )
}

fn append_hold_backed_timed_input(
    nodes: &mut Vec<IrNode>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    spec: SumOfStepsSpec,
    tick_port: SourcePortId,
    sink_port: SinkPortId,
    base_node_id: u32,
    persistence: LoweringPathPersistenceSeed<'_>,
) -> Vec<IrNodePersistence> {
    let hold = append_accumulating_timed_input(nodes, spec, tick_port, sink_port, base_node_id);

    collect_path_lowering_persistence_from_seed(bindings, persistence, hold)
}

fn append_hold_result_sink(
    nodes: &mut Vec<IrNode>,
    updates: NodeId,
    result_sink: SinkPortId,
    base_node_id: u32,
) {
    let skip = NodeId(base_node_id);
    nodes.push(IrNode {
        id: skip,
        source_expr: None,
        kind: IrNodeKind::Skip,
    });
    append_seeded_hold_sink(
        nodes,
        skip,
        updates,
        base_node_id + 1,
        result_sink,
        base_node_id + 2,
    );
}

fn append_updates_hold(nodes: &mut Vec<IrNode>, updates: NodeId, base_node_id: u32) -> NodeId {
    let skip = NodeId(base_node_id);
    nodes.push(IrNode {
        id: skip,
        source_expr: None,
        kind: IrNodeKind::Skip,
    });
    append_seeded_hold(nodes, skip, updates, base_node_id + 1)
}

fn timed_math_action_tag(kind: TimedMathActionKind) -> KernelValue {
    match kind {
        TimedMathActionKind::Add => KernelValue::Tag("Addition".to_string()),
        TimedMathActionKind::Sub => KernelValue::Tag("Subtraction".to_string()),
    }
}

fn append_timed_math_action_tags(
    nodes: &mut Vec<IrNode>,
    actions: &[TimedMathActionConfig],
    base_node_id: u32,
) -> (NodeId, u32) {
    let mut tag_updates = Vec::with_capacity(actions.len());

    for (index, action) in actions.iter().enumerate() {
        let node_base = base_node_id + index as u32 * 3;
        tag_updates.push(SourceTriggeredLiteralConfig {
            source_port: action.press_port,
            literal: timed_math_action_tag(action.kind),
            source_node_id: node_base,
            literal_node_id: node_base + 1,
            then_node_id: node_base + 2,
        });
    }

    let updates = append_source_triggered_literal_updates(
        nodes,
        &tag_updates,
        base_node_id + actions.len() as u32 * 3,
        LatestHoldMode::OnlyWhenMultiple,
    );
    let next_node_id = if actions.len() == 1 {
        base_node_id + actions.len() as u32 * 3
    } else {
        updates.0 + 1
    };
    (updates, next_node_id)
}

fn append_timed_math_operation_nodes(
    nodes: &mut Vec<IrNode>,
    input_a_hold: NodeId,
    input_b_hold: NodeId,
    action_kinds: &[TimedMathActionKind],
    base_node_id: u32,
) -> TimedMathOperationNodes {
    let mut next_node_id = base_node_id;
    let add = if action_kinds.contains(&TimedMathActionKind::Add) {
        let node = NodeId(next_node_id);
        next_node_id += 1;
        nodes.push(IrNode {
            id: node,
            source_expr: None,
            kind: IrNodeKind::Add {
                lhs: input_a_hold,
                rhs: input_b_hold,
            },
        });
        Some(node)
    } else {
        None
    };
    let sub = if action_kinds.contains(&TimedMathActionKind::Sub) {
        let node = NodeId(next_node_id);
        next_node_id += 1;
        nodes.push(IrNode {
            id: node,
            source_expr: None,
            kind: IrNodeKind::Sub {
                lhs: input_a_hold,
                rhs: input_b_hold,
            },
        });
        Some(node)
    } else {
        None
    };

    TimedMathOperationNodes {
        add,
        sub,
        next_node_id,
    }
}

fn append_timed_math_operation_choice(
    nodes: &mut Vec<IrNode>,
    source: NodeId,
    operations: &TimedMathOperationNodes,
    fallback: NodeId,
    base_node_id: u32,
) -> NodeId {
    let choice = NodeId(base_node_id);
    let mut arms = Vec::new();
    if let Some(add) = operations.add {
        arms.push(crate::ir::MatchArm {
            matcher: timed_math_action_tag(TimedMathActionKind::Add),
            result: add,
        });
    }
    if let Some(sub) = operations.sub {
        arms.push(crate::ir::MatchArm {
            matcher: timed_math_action_tag(TimedMathActionKind::Sub),
            result: sub,
        });
    }
    append_while(nodes, source, arms, fallback, choice.0)
}

fn append_timed_math_input_hold(
    nodes: &mut Vec<IrNode>,
    bindings: Option<&BTreeMap<String, &StaticSpannedExpression>>,
    persistence: Option<LoweringPathPersistenceSeed<'_>>,
    spec: SumOfStepsSpec,
    tick_port: SourcePortId,
    sink_port: SinkPortId,
    base_node_id: u32,
) -> (NodeId, Vec<IrNodePersistence>) {
    match (bindings, persistence) {
        (Some(bindings), Some(persistence)) => {
            let persistence = append_hold_backed_timed_input(
                nodes,
                bindings,
                spec,
                tick_port,
                sink_port,
                base_node_id,
                persistence,
            );
            (NodeId(base_node_id + 5), persistence)
        }
        _ => (
            append_accumulating_timed_input(nodes, spec, tick_port, sink_port, base_node_id),
            Vec::new(),
        ),
    }
}

fn extract_sum_of_steps_call(
    expression: &StaticSpannedExpression,
    subset: &str,
) -> Result<SumOfStepsSpec, String> {
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Err(format!(
            "{subset} subset requires `sum_of_steps(step: ..., seconds: ...)`"
        ));
    };
    if !path_matches(path, &["sum_of_steps"]) {
        return Err(format!(
            "{subset} subset requires `sum_of_steps(step: ..., seconds: ...)`"
        ));
    }
    let step = find_named_argument(arguments, "step")
        .ok_or_else(|| format!("{subset} subset requires `step` argument for `sum_of_steps`"))
        .and_then(extract_integer_literal)?;
    let seconds = find_named_argument(arguments, "seconds")
        .ok_or_else(|| format!("{subset} subset requires `seconds` argument for `sum_of_steps`"))
        .and_then(extract_number_literal)?;

    Ok(SumOfStepsSpec {
        step,
        interval_ms: (seconds * 1000.0).round() as u32,
    })
}

fn lower_hold_backed_timed_math_mode_ir(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    input_a_persistence: LoweringPathPersistenceSeed<'_>,
    input_b_persistence: LoweringPathPersistenceSeed<'_>,
    input_a: SumOfStepsSpec,
    input_b: SumOfStepsSpec,
    input_a_tick_port: SourcePortId,
    input_b_tick_port: SourcePortId,
    input_a_sink: SinkPortId,
    input_b_sink: SinkPortId,
    mode: HoldBackedTimedMathIrMode,
    result_sink: SinkPortId,
) -> IrProgram {
    match mode {
        HoldBackedTimedMathIrMode::Then {
            addition_press_port,
        } => lower_timed_math_ir(
            Some(bindings),
            Some(input_a_persistence),
            Some(input_b_persistence),
            input_a,
            input_b,
            input_a_tick_port,
            input_b_tick_port,
            input_a_sink,
            input_b_sink,
            &[TimedMathActionConfig {
                press_port: addition_press_port,
                kind: TimedMathActionKind::Add,
            }],
            TimedMathActionSourceMode::LatestUpdates,
            TimedMathResultMode::Hold,
            result_sink,
            2100,
        ),
        HoldBackedTimedMathIrMode::When {
            addition_press_port,
            subtraction_press_port,
        } => lower_timed_math_ir(
            Some(bindings),
            Some(input_a_persistence),
            Some(input_b_persistence),
            input_a,
            input_b,
            input_a_tick_port,
            input_b_tick_port,
            input_a_sink,
            input_b_sink,
            &[
                TimedMathActionConfig {
                    press_port: addition_press_port,
                    kind: TimedMathActionKind::Add,
                },
                TimedMathActionConfig {
                    press_port: subtraction_press_port,
                    kind: TimedMathActionKind::Sub,
                },
            ],
            TimedMathActionSourceMode::LatestUpdates,
            TimedMathResultMode::Hold,
            result_sink,
            2200,
        ),
    }
}

fn lower_timed_math_ir(
    bindings: Option<&BTreeMap<String, &StaticSpannedExpression>>,
    input_a_persistence: Option<LoweringPathPersistenceSeed<'_>>,
    input_b_persistence: Option<LoweringPathPersistenceSeed<'_>>,
    input_a: SumOfStepsSpec,
    input_b: SumOfStepsSpec,
    input_a_tick_port: SourcePortId,
    input_b_tick_port: SourcePortId,
    input_a_sink: SinkPortId,
    input_b_sink: SinkPortId,
    actions: &[TimedMathActionConfig],
    action_source_mode: TimedMathActionSourceMode,
    result_mode: TimedMathResultMode,
    result_sink: SinkPortId,
    base_node_id: u32,
) -> IrProgram {
    let mut nodes = Vec::new();
    let (input_a_hold, input_a_persistence) = append_timed_math_input_hold(
        &mut nodes,
        bindings,
        input_a_persistence,
        input_a,
        input_a_tick_port,
        input_a_sink,
        base_node_id,
    );
    let (input_b_hold, input_b_persistence) = append_timed_math_input_hold(
        &mut nodes,
        bindings,
        input_b_persistence,
        input_b,
        input_b_tick_port,
        input_b_sink,
        base_node_id + 7,
    );

    let action_base_node_id = base_node_id + 14;
    let action_kinds = actions.iter().map(|action| action.kind).collect::<Vec<_>>();
    let (action_source, next_node_id, fallback) = match action_source_mode {
        TimedMathActionSourceMode::LatestUpdates => {
            let (action_updates, next_node_id) =
                append_timed_math_action_tags(&mut nodes, actions, action_base_node_id);
            let fallback = NodeId(next_node_id);
            nodes.push(IrNode {
                id: fallback,
                source_expr: None,
                kind: IrNodeKind::Skip,
            });
            (action_updates, next_node_id + 1, fallback)
        }
        TimedMathActionSourceMode::HeldSelection => {
            let (action_updates, next_node_id) =
                append_timed_math_action_tags(&mut nodes, actions, action_base_node_id);
            let selected_operation = append_updates_hold(&mut nodes, action_updates, next_node_id);
            let fallback = NodeId(next_node_id);
            (selected_operation, next_node_id + 2, fallback)
        }
    };
    let operations = append_timed_math_operation_nodes(
        &mut nodes,
        input_a_hold,
        input_b_hold,
        &action_kinds,
        next_node_id,
    );
    let updates = append_timed_math_operation_choice(
        &mut nodes,
        action_source,
        &operations,
        fallback,
        operations.next_node_id,
    );

    match result_mode {
        TimedMathResultMode::Hold => {
            append_hold_result_sink(
                &mut nodes,
                updates,
                result_sink,
                operations.next_node_id + 1,
            );
        }
        TimedMathResultMode::Direct => {
            append_value_sink(
                &mut nodes,
                updates,
                result_sink,
                operations.next_node_id + 1,
            );
        }
    }

    IrProgram {
        nodes,
        functions: Vec::new(),
        persistence: input_a_persistence
            .into_iter()
            .chain(input_b_persistence)
            .collect(),
    }
}

define_source_parsed_entrypoint!(
    try_lower_while_function_call,
    try_lower_while_function_call_from_expressions,
    WhileFunctionCallProgram
);

fn try_lower_while_function_call_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<WhileFunctionCallProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_SURFACE_GROUP,
        "toggle_branch_document",
        |program| match program {
            LoweredProgram::WhileFunctionCall(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_button_hover_to_click_test,
    try_lower_button_hover_to_click_test_from_expressions,
    ButtonHoverToClickTestProgram
);

fn try_lower_button_hover_to_click_test_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ButtonHoverToClickTestProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_SURFACE_GROUP,
        "multi_button_activation_document",
        |program| match program {
            LoweredProgram::ButtonHoverToClickTest(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_button_hover_test,
    try_lower_button_hover_test_from_expressions,
    ButtonHoverTestProgram
);

fn try_lower_button_hover_test_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<ButtonHoverTestProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_SURFACE_GROUP,
        "multi_button_hover_document",
        |program| match program {
            LoweredProgram::ButtonHoverTest(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_switch_hold_test,
    try_lower_switch_hold_test_from_expressions,
    SwitchHoldTestProgram
);

fn try_lower_switch_hold_test_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<SwitchHoldTestProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_SURFACE_GROUP,
        "switched_hold_items_document",
        |program| match program {
            LoweredProgram::SwitchHoldTest(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_circle_drawer,
    try_lower_circle_drawer_from_expressions,
    CircleDrawerProgram
);

fn try_lower_circle_drawer_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<CircleDrawerProgram, String> {
    lower_surface_group_typed_program(
        expressions,
        &GENERIC_HOST_IR_SURFACE_GROUP,
        "canvas_history_document",
        |program| match program {
            LoweredProgram::CircleDrawer(program) => Some(program),
            _ => None,
        },
    )
}

define_source_parsed_entrypoint!(
    try_lower_static_document,
    try_lower_static_document_from_expressions,
    StaticProgram
);

fn try_lower_static_document_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<StaticProgram, String> {
    lower_bindings_group_typed_program(
        expressions,
        &DISPLAY_SINGLE_SINK_BINDINGS_GROUP,
        "static_document_display",
        |program| match program {
            LoweredProgram::StaticDocument(program) => Some(program),
            _ => None,
        },
    )
}

fn lower_increment_button(
    expression: &StaticSpannedExpression,
) -> Result<(SourcePortId, String), String> {
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Err("counter subset requires increment_button to be Element/button".to_string());
    };
    if !path_matches(path, &["Element", "button"]) {
        return Err("counter subset requires increment_button to be Element/button".to_string());
    }

    let element_argument = find_named_argument(arguments, "element")
        .ok_or_else(|| "Element/button requires `element` argument".to_string())?;
    let label_argument = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/button requires `label` argument".to_string())?;

    let press_port = extract_press_link(element_argument)?;
    let label_text = extract_button_label(label_argument)?;
    Ok((press_port, label_text))
}

struct NumericAccumulatorAction {
    press_port: SourcePortId,
    delta: i64,
}

struct NumericAccumulatorMirrorSink {
    cell: MirrorCellId,
    sink: SinkPortId,
}

struct NumericAccumulatorRuntimeConfig<'a> {
    base_node_id: u32,
    initial_value: i64,
    actions: &'a [NumericAccumulatorAction],
    counter_sink: SinkPortId,
    mirror_sinks: &'a [NumericAccumulatorMirrorSink],
}

struct NumericAccumulatorRuntime {
    nodes: Vec<IrNode>,
    hold_node: NodeId,
}

struct SingleActionAccumulatorSemanticLowering {
    initial_value: i64,
    increment_delta: i64,
    persist_hold: bool,
}

fn lower_numeric_accumulator_program(
    config: &NumericAccumulatorRuntimeConfig<'_>,
    build_persistence: impl FnOnce(NodeId) -> Vec<IrNodePersistence>,
) -> IrProgram {
    let runtime = lower_numeric_accumulator_runtime(config);
    let persistence = build_persistence(runtime.hold_node);

    IrProgram {
        nodes: runtime.nodes,
        functions: Vec::new(),
        persistence,
    }
}

fn lower_numeric_accumulator_runtime(
    config: &NumericAccumulatorRuntimeConfig<'_>,
) -> NumericAccumulatorRuntime {
    let base = config.base_node_id;
    let action_count = config.actions.len() as u32;
    let latest_offset = if action_count > 1 { 1 } else { 0 };
    let hold_node = NodeId(base + 1 + 4 * action_count + latest_offset);

    let mut nodes = Vec::new();
    append_literal(
        &mut nodes,
        KernelValue::from(config.initial_value as f64),
        base,
    );

    let mut actions = Vec::with_capacity(config.actions.len());
    for (index, action) in config.actions.iter().enumerate() {
        let action_base = base + 1 + index as u32 * 4;
        actions.push(SourceDeltaAccumulatorConfig {
            source_port: action.press_port,
            delta: action.delta as f64,
            source_node_id: action_base,
            delta_node_id: action_base + 1,
            sum_node_id: action_base + 2,
            then_node_id: action_base + 3,
        });
    }

    append_source_delta_accumulator_hold_sink(
        &mut nodes,
        NodeId(base),
        hold_node.0,
        &actions,
        base + 1 + 4 * action_count,
        LatestHoldMode::OnlyWhenMultiple,
        config.counter_sink,
        hold_node.0 + 1,
    );

    let mut next_node_id = hold_node.0 + 2;
    for mirror in config.mirror_sinks {
        let mirror_node = NodeId(next_node_id);
        next_node_id += 1;
        append_mirror_cell(&mut nodes, mirror.cell, mirror_node.0);
        append_value_sink(&mut nodes, mirror_node, mirror.sink, next_node_id);
        next_node_id += 1;
    }

    NumericAccumulatorRuntime { nodes, hold_node }
}

fn lower_single_action_numeric_accumulator_program<'a>(
    initial_value: i64,
    increment_delta: i64,
    press_port: SourcePortId,
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    hold_persistence: Option<LoweringPathPersistenceSeed<'_>>,
    persist_hold: bool,
) -> IrProgram {
    let config = NumericAccumulatorRuntimeConfig {
        base_node_id: 1,
        initial_value,
        actions: &[NumericAccumulatorAction {
            press_port,
            delta: increment_delta,
        }],
        counter_sink: SinkPortId(1),
        mirror_sinks: &[],
    };

    lower_numeric_accumulator_program(&config, |hold_node| {
        match (persist_hold, hold_persistence) {
            (true, Some(seed)) => {
                collect_path_lowering_persistence_from_seed(bindings, seed, hold_node)
            }
            _ => Vec::new(),
        }
    })
}

fn lower_single_action_accumulator<'a>(
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    expression: &StaticSpannedExpression,
    expected_press_port: SourcePortId,
    config: &PressDrivenAccumulatorSemanticConfig<'_>,
) -> Result<(i64, i64, IrProgram), String> {
    let lowering =
        derive_single_action_accumulator_semantics(expression, expected_press_port, config)?;
    let ir = lower_single_action_numeric_accumulator_program(
        lowering.initial_value,
        lowering.increment_delta,
        expected_press_port,
        bindings,
        config.hold_persistence,
        lowering.persist_hold,
    );

    Ok((lowering.initial_value, lowering.increment_delta, ir))
}

fn derive_single_action_accumulator_semantics<'a>(
    expression: &'a StaticSpannedExpression,
    expected_press_port: SourcePortId,
    config: &PressDrivenAccumulatorSemanticConfig<'_>,
) -> Result<SingleActionAccumulatorSemanticLowering, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(format!(
            "{} subset requires `LATEST {{ ... }} |> Math/sum()` or `HOLD`",
            config.subset
        ));
    };
    let _ = (from, to);

    for shape in config.shapes {
        let lowering = match shape {
            SingleActionAccumulatorSemanticShape::LatestSum => {
                derive_latest_sum_single_action_accumulator_semantics(
                    expression,
                    expected_press_port,
                    config.subset,
                )?
            }
            SingleActionAccumulatorSemanticShape::Hold => {
                derive_hold_single_action_accumulator_semantics(
                    expression,
                    expected_press_port,
                    config.subset,
                )?
            }
        };
        if let Some(lowering) = lowering {
            return Ok(lowering);
        }
    }

    Err(format!(
        "{} subset requires `LATEST {{ ... }} |> Math/sum()` or `HOLD`",
        config.subset
    ))
}

fn derive_latest_sum_single_action_accumulator_semantics<'a>(
    expression: &'a StaticSpannedExpression,
    expected_press_port: SourcePortId,
    subset: &'static str,
) -> Result<Option<SingleActionAccumulatorSemanticLowering>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(format!(
            "{subset} subset requires `LATEST {{ ... }} |> Math/sum()` or `HOLD`"
        ));
    };

    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Math", "sum"]) || !arguments.is_empty() {
        return Err(format!("{subset} subset requires `Math/sum()` or `HOLD`"));
    }

    let StaticExpression::Latest { inputs } = &from.node else {
        return Err(format!("{subset} subset requires `LATEST` before Math/sum"));
    };
    if inputs.len() != 2 {
        return Err(format!("{subset} subset requires two LATEST inputs"));
    }

    let initial_value = extract_integer_literal(&inputs[0])?;
    let increment_delta =
        extract_then_increment_delta(&inputs[1], expected_press_port, None, subset)?;

    Ok(Some(SingleActionAccumulatorSemanticLowering {
        initial_value,
        increment_delta,
        persist_hold: true,
    }))
}

fn derive_hold_single_action_accumulator_semantics<'a>(
    expression: &'a StaticSpannedExpression,
    expected_press_port: SourcePortId,
    subset: &'static str,
) -> Result<Option<SingleActionAccumulatorSemanticLowering>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(format!(
            "{subset} subset requires `LATEST {{ ... }} |> Math/sum()` or `HOLD`"
        ));
    };

    let StaticExpression::Hold { state_param, body } = &to.node else {
        return Ok(None);
    };

    let initial_value = extract_integer_literal(from)?;
    let increment_delta = extract_then_increment_delta(
        body,
        expected_press_port,
        Some(state_param.as_str()),
        subset,
    )?;

    Ok(Some(SingleActionAccumulatorSemanticLowering {
        initial_value,
        increment_delta,
        persist_hold: true,
    }))
}

fn extract_then_increment_delta(
    expression: &StaticSpannedExpression,
    expected_press_port: SourcePortId,
    state_param: Option<&str>,
    subset: &str,
) -> Result<i64, String> {
    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &expression.node
    else {
        return Err(format!("{subset} subset requires event THEN branch"));
    };
    let press_port = extract_event_press_port(trigger_source)?;
    if press_port != expected_press_port {
        return Err(format!(
            "{subset} subset requires button LINK and accumulator trigger to share press port"
        ));
    }
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Err(format!("{subset} subset requires THEN body"));
    };
    match (&body.node, state_param) {
        (_, None) => extract_integer_literal(body),
        (StaticExpression::ArithmeticOperator(operator), Some(state_param)) => {
            extract_hold_state_increment_delta(operator, state_param, subset)
        }
        _ => Err(format!(
            "{subset} HOLD subset requires `<state> + <integer>`"
        )),
    }
}

fn extract_hold_state_increment_delta(
    operator: &boon::parser::static_expression::ArithmeticOperator,
    state_param: &str,
    subset: &str,
) -> Result<i64, String> {
    let boon::parser::static_expression::ArithmeticOperator::Add {
        operand_a,
        operand_b,
    } = operator
    else {
        return Err(format!(
            "{subset} HOLD subset requires `{state_param} + <integer>`"
        ));
    };

    let StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
        parts,
        ..
    }) = &operand_a.node
    else {
        return Err(format!(
            "{subset} HOLD subset requires state param on left side"
        ));
    };
    if parts.len() != 1 || parts[0].as_str() != state_param {
        return Err(format!(
            "{subset} HOLD subset requires state param on left side"
        ));
    }

    extract_integer_literal(operand_b)
}

fn styled_stripe_layout(
    direction: HostStripeDirection,
    gap_px: u32,
    padding_px: Option<u32>,
    width: Option<HostWidth>,
    align_cross: Option<HostCrossAlign>,
) -> HostViewKind {
    HostViewKind::StripeLayout {
        direction,
        gap_px,
        padding_px,
        width,
        align_cross,
    }
}

fn styled_label(
    sink: SinkPortId,
    font_size_px: Option<u32>,
    bold: bool,
    color: Option<&str>,
) -> HostViewKind {
    HostViewKind::StyledLabel {
        sink,
        font_size_px,
        bold,
        color: color.map(str::to_string),
    }
}

fn styled_text_input(
    value_sink: SinkPortId,
    placeholder: &str,
    change_port: SourcePortId,
    key_down_port: SourcePortId,
    focus_on_mount: bool,
    disabled_sink: Option<SinkPortId>,
    width: Option<HostWidth>,
) -> HostViewKind {
    HostViewKind::StyledTextInput {
        value_sink,
        placeholder: placeholder.to_string(),
        change_port,
        key_down_port,
        blur_port: None,
        focus_port: None,
        focus_on_mount,
        disabled_sink,
        width,
    }
}

fn styled_select(
    selected_sink: SinkPortId,
    change_port: SourcePortId,
    options: Vec<HostSelectOption>,
    disabled_sink: Option<SinkPortId>,
    width: Option<HostWidth>,
) -> HostViewKind {
    HostViewKind::StyledSelect {
        selected_sink,
        change_port,
        options,
        disabled_sink,
        width,
    }
}

fn styled_slider(
    value_sink: SinkPortId,
    input_port: SourcePortId,
    min: &str,
    max: &str,
    step: &str,
    disabled_sink: Option<SinkPortId>,
    width: Option<HostWidth>,
) -> HostViewKind {
    HostViewKind::StyledSlider {
        value_sink,
        input_port,
        min: min.to_string(),
        max: max.to_string(),
        step: step.to_string(),
        disabled_sink,
        width,
    }
}

fn styled_button(
    label: &str,
    press_port: SourcePortId,
    disabled_sink: Option<SinkPortId>,
    width: Option<HostWidth>,
    padding_px: Option<u32>,
    rounded_fully: bool,
    background: Option<&str>,
    background_sink: Option<SinkPortId>,
    active_background: Option<&str>,
    outline_sink: Option<SinkPortId>,
    active_outline: Option<&str>,
) -> HostViewKind {
    HostViewKind::StyledButton {
        label: HostButtonLabel::Static(label.to_string()),
        press_port,
        disabled_sink,
        width,
        padding_px,
        rounded_fully,
        background: background.map(str::to_string),
        background_sink,
        active_background: active_background.map(str::to_string),
        outline_sink,
        active_outline: active_outline.map(str::to_string),
    }
}

fn styled_action_label(
    sink: SinkPortId,
    press_port: SourcePortId,
    width: Option<HostWidth>,
    bold_sink: Option<SinkPortId>,
) -> HostViewKind {
    HostViewKind::StyledActionLabel {
        sink,
        press_port,
        event_kind: UiEventKind::Click,
        width,
        bold_sink,
    }
}

fn lower_timed_counter_signal_ir(
    spec: SumOfStepsSpec,
    tick_port: SourcePortId,
    value_sink: SinkPortId,
    base_node_id: u32,
    mode: TimedCounterOutputMode,
) -> Vec<IrNode> {
    match mode {
        TimedCounterOutputMode::SummedUpdates => {
            let mut nodes = Vec::new();
            let tick = append_source_port(&mut nodes, tick_port, base_node_id);
            append_literal(
                &mut nodes,
                KernelValue::from(spec.step as f64),
                base_node_id + 1,
            );
            append_then(&mut nodes, tick, NodeId(base_node_id + 1), base_node_id + 2);
            nodes.push(IrNode {
                id: NodeId(base_node_id + 3),
                source_expr: None,
                kind: IrNodeKind::MathSum {
                    input: NodeId(base_node_id + 2),
                },
            });
            append_value_sink(
                &mut nodes,
                NodeId(base_node_id + 3),
                value_sink,
                base_node_id + 4,
            );
            nodes
        }
        TimedCounterOutputMode::HoldOnTick => {
            let mut nodes = Vec::new();
            let (tick_source, hold) =
                append_accumulating_timed_hold(&mut nodes, spec, tick_port, base_node_id);
            append_then(&mut nodes, tick_source, hold, base_node_id + 6);
            append_value_sink(
                &mut nodes,
                NodeId(base_node_id + 6),
                value_sink,
                base_node_id + 7,
            );
            nodes
        }
    }
}

fn lower_interval_document_expression(expression: &StaticSpannedExpression) -> Result<u32, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err("interval subset requires piped `document`".to_string());
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err("interval subset requires `|> Document/new()`".to_string());
    };
    if !path_matches(path, &["Document", "new"]) || !arguments.is_empty() {
        return Err("interval subset requires `|> Document/new()`".to_string());
    }
    lower_interval_sum_expression(from)
}

fn lower_interval_sum_expression(expression: &StaticSpannedExpression) -> Result<u32, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err("interval subset requires `|> Math/sum()`".to_string());
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err("interval subset requires `|> Math/sum()`".to_string());
    };
    if !path_matches(path, &["Math", "sum"]) || !arguments.is_empty() {
        return Err("interval subset requires `|> Math/sum()`".to_string());
    }
    lower_interval_then_expression(from)
}

fn lower_interval_then_expression(expression: &StaticSpannedExpression) -> Result<u32, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err("interval subset requires `|> THEN { 1 }`".to_string());
    };
    let StaticExpression::Then { body } = &to.node else {
        return Err("interval subset requires `|> THEN { 1 }`".to_string());
    };
    if extract_integer_literal(body)? != 1 {
        return Err("interval subset requires THEN body `1`".to_string());
    }
    extract_timer_interval_ms(from)
}

fn extract_timer_interval_ms(expression: &StaticSpannedExpression) -> Result<u32, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err("interval subset requires `Duration[...] |> Timer/interval()`".to_string());
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err("interval subset requires `Timer/interval()`".to_string());
    };
    if !path_matches(path, &["Timer", "interval"]) || !arguments.is_empty() {
        return Err("interval subset requires `Timer/interval()`".to_string());
    }
    extract_duration_ms(from)
}

fn extract_duration_ms(expression: &StaticSpannedExpression) -> Result<u32, String> {
    let arguments = match &expression.node {
        StaticExpression::TaggedObject { tag, object } if tag.as_str() == "Duration" => object
            .variables
            .iter()
            .map(|variable| boon::parser::static_expression::Spanned {
                node: boon::parser::static_expression::Argument {
                    name: variable.node.name.clone(),
                    is_referenced: variable.node.is_referenced,
                    value: Some(variable.node.value.clone()),
                },
                span: variable.span,
                persistence: None,
            })
            .collect::<Vec<_>>(),
        StaticExpression::FunctionCall { path, arguments } if path_matches(path, &["Duration"]) => {
            arguments.clone()
        }
        _ => {
            return Err("interval subset requires `Duration[...]`".to_string());
        }
    };

    if arguments.is_empty() {
        return Err("interval subset requires `Duration[...]`".to_string());
    }

    if let Some(seconds) = find_named_argument(&arguments, "seconds") {
        let seconds = extract_number_literal(seconds)?;
        return Ok((seconds * 1000.0).round() as u32);
    }
    if let Some(milliseconds) = find_named_argument(&arguments, "milliseconds") {
        let milliseconds = extract_number_literal(milliseconds)?;
        return Ok(milliseconds.round() as u32);
    }

    Err("interval subset requires `seconds` or `milliseconds` duration".to_string())
}

fn lower_interval_hold_counter(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(
            "held_interval_signal_document subset requires `HOLD ... |> Stream/skip(count: 1)`"
                .to_string(),
        );
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err(
            "held_interval_signal_document subset requires `Stream/skip(count: 1)`".to_string(),
        );
    };
    if !path_matches(path, &["Stream", "skip"]) {
        return Err(
            "held_interval_signal_document subset requires `Stream/skip(count: 1)`".to_string(),
        );
    }
    let count = find_named_argument(arguments, "count").ok_or_else(|| {
        "held_interval_signal_document subset requires `count` for Stream/skip".to_string()
    })?;
    if extract_integer_literal(count)? != 1 {
        return Err(
            "held_interval_signal_document subset requires `Stream/skip(count: 1)`".to_string(),
        );
    }

    let StaticExpression::Pipe {
        from: seed,
        to: hold,
    } = &from.node
    else {
        return Err(
            "held_interval_signal_document subset requires `0 |> HOLD counter { ... }`".to_string(),
        );
    };
    if extract_integer_literal(seed)? != 0 {
        return Err(
            "held_interval_signal_document subset requires `0 |> HOLD counter { ... }`".to_string(),
        );
    }

    let StaticExpression::Hold { state_param, body } = &hold.node else {
        return Err(
            "held_interval_signal_document subset requires `HOLD counter { ... }`".to_string(),
        );
    };
    if state_param.as_str() != "counter" {
        return Err(
            "held_interval_signal_document subset requires HOLD state param `counter`".to_string(),
        );
    }

    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &body.node
    else {
        return Err(
            "held_interval_signal_document subset requires `tick |> THEN { counter + 1 }`"
                .to_string(),
        );
    };
    ensure_alias_name(trigger_source, "tick").map_err(|_| {
        "held_interval_signal_document subset requires `tick |> THEN { counter + 1 }`".to_string()
    })?;
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Err("held_interval_signal_document subset requires THEN body".to_string());
    };
    match &body.node {
        StaticExpression::ArithmeticOperator(operator) => {
            let increment = extract_hold_state_increment_delta(
                operator,
                "counter",
                "held_interval_signal_document",
            )?;
            if increment != 1 {
                return Err(
                    "held_interval_signal_document subset requires `counter + 1`".to_string(),
                );
            }
        }
        _ => return Err("held_interval_signal_document subset requires `counter + 1`".to_string()),
    }

    Ok(())
}

fn lower_interval_hold_document(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(
            "held_interval_signal_document subset requires `counter |> Document/new()`".to_string(),
        );
    };
    ensure_alias_name(from, "counter").map_err(|_| {
        "held_interval_signal_document subset requires `counter |> Document/new()`".to_string()
    })?;
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err("held_interval_signal_document subset requires `Document/new()`".to_string());
    };
    if !path_matches(path, &["Document", "new"]) || !arguments.is_empty() {
        return Err("held_interval_signal_document subset requires `Document/new()`".to_string());
    }
    Ok(())
}

fn ensure_sequence_function(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Function {
        name,
        parameters,
        body: _,
    } = &expression.node
    else {
        return Err("fibonacci subset requires `FUNCTION fibonacci(position)`".to_string());
    };
    if name.as_str() != "fibonacci" {
        return Err("fibonacci subset requires `FUNCTION fibonacci(position)`".to_string());
    }
    if parameters.len() != 1 || parameters[0].node.as_str() != "position" {
        return Err("fibonacci subset requires one `position` parameter".to_string());
    }
    Ok(())
}

fn ensure_sequence_result_binding(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(
            "fibonacci subset requires `position |> fibonacci()` result binding".to_string(),
        );
    };
    ensure_alias_name(from, "position").map_err(|_| {
        "fibonacci subset requires `position |> fibonacci()` result binding".to_string()
    })?;
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err(
            "fibonacci subset requires `position |> fibonacci()` result binding".to_string(),
        );
    };
    if path.len() != 1 || path[0].as_str() != "fibonacci" || !arguments.is_empty() {
        return Err(
            "fibonacci subset requires `position |> fibonacci()` result binding".to_string(),
        );
    }
    Ok(())
}

fn ensure_sequence_message(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::TextLiteral { parts, .. } = &expression.node else {
        return Err("fibonacci subset requires text-literal `message`".to_string());
    };
    if parts.len() != 3 {
        return Err("fibonacci subset requires interpolated `message`".to_string());
    }
    match (&parts[0], &parts[1], &parts[2]) {
        (
            boon::parser::static_expression::TextPart::Interpolation { var, .. },
            boon::parser::static_expression::TextPart::Text(text),
            boon::parser::static_expression::TextPart::Interpolation { var: result, .. },
        ) if var.as_str() == "position"
            && text.as_str() == ". Fibonacci number is "
            && result.as_str() == "result" =>
        {
            Ok(())
        }
        _ => Err(
            "fibonacci subset requires `TEXT { {position}. Fibonacci number is {result} }`"
                .to_string(),
        ),
    }
}

fn ensure_sequence_document(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err("fibonacci subset requires `message |> Document/new()`".to_string());
    };
    ensure_alias_name(from, "message")
        .map_err(|_| "fibonacci subset requires `message |> Document/new()`".to_string())?;
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err("fibonacci subset requires `Document/new()`".to_string());
    };
    if !path_matches(path, &["Document", "new"]) || !arguments.is_empty() {
        return Err("fibonacci subset requires `Document/new()`".to_string());
    }
    Ok(())
}

fn compute_sequence_number(position: u64) -> u64 {
    match position {
        0 => 0,
        1 => 1,
        n => {
            let mut previous = 0u64;
            let mut current = 1u64;
            for _ in 1..n {
                let next = previous + current;
                previous = current;
                current = next;
            }
            current
        }
    }
}

#[derive(Clone, Copy)]
enum MappedLabelNodeKind {
    Plain,
}

fn lower_mapped_label_nodes(
    function_instance: Option<FunctionInstanceId>,
    view_site: ViewSiteId,
    sink_start: u32,
    count: u32,
    label_kind: &MappedLabelNodeKind,
) -> Vec<HostViewNode> {
    (0..count)
        .map(|index| HostViewNode {
            retained_key: RetainedNodeKey {
                view_site,
                function_instance,
                mapped_item_identity: Some((index + 1).into()),
            },
            kind: match label_kind {
                MappedLabelNodeKind::Plain => HostViewKind::Label {
                    sink: SinkPortId(sink_start + index),
                },
            },
            children: Vec::new(),
        })
        .collect()
}

#[derive(Clone, Copy)]
struct MappedLabelListConfig {
    function_instance: Option<FunctionInstanceId>,
    list_view_site: ViewSiteId,
    list_item_view_site: ViewSiteId,
    sink_start: u32,
    count: u32,
    label_kind: MappedLabelNodeKind,
}

fn lower_mapped_label_list(config: &MappedLabelListConfig) -> HostViewNode {
    HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: config.list_view_site,
            function_instance: config.function_instance,
            mapped_item_identity: None,
        },
        kind: HostViewKind::Stripe,
        children: lower_mapped_label_nodes(
            config.function_instance,
            config.list_item_view_site,
            config.sink_start,
            config.count,
            &config.label_kind,
        ),
    }
}

#[derive(Clone, Copy)]
enum FlatStripeDocumentChildConfig<'a> {
    Label {
        view_site: ViewSiteId,
        sink: SinkPortId,
    },
    Button {
        view_site: ViewSiteId,
        label: &'a str,
        press_port: SourcePortId,
    },
    TextInput {
        view_site: ViewSiteId,
        value_sink: SinkPortId,
        placeholder: &'a str,
        change_port: SourcePortId,
        key_down_port: SourcePortId,
        focus_on_mount: bool,
    },
    MappedLabelList(MappedLabelListConfig),
    MappedButtonLabelRows {
        container_view_site: ViewSiteId,
        rows: MappedButtonLabelRowsConfig<'a>,
    },
}

#[derive(Clone, Copy)]
struct FlatStripeDocumentConfig<'a> {
    function_instance: FunctionInstanceId,
    root_view_site: ViewSiteId,
    stripe_view_site: ViewSiteId,
    children: &'a [FlatStripeDocumentChildConfig<'a>],
}

fn lower_flat_stripe_document(config: &FlatStripeDocumentConfig<'_>) -> HostViewIr {
    let function_instance = Some(config.function_instance);
    let stripe_children = config
        .children
        .iter()
        .map(|child| match child {
            FlatStripeDocumentChildConfig::Label { view_site, sink } => HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: *view_site,
                    function_instance,
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Label { sink: *sink },
                children: Vec::new(),
            },
            FlatStripeDocumentChildConfig::Button {
                view_site,
                label,
                press_port,
            } => HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: *view_site,
                    function_instance,
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Button {
                    label: HostButtonLabel::Static((*label).to_string()),
                    press_port: *press_port,
                    disabled_sink: None,
                },
                children: Vec::new(),
            },
            FlatStripeDocumentChildConfig::TextInput {
                view_site,
                value_sink,
                placeholder,
                change_port,
                key_down_port,
                focus_on_mount,
            } => HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: *view_site,
                    function_instance,
                    mapped_item_identity: None,
                },
                kind: HostViewKind::TextInput {
                    value_sink: *value_sink,
                    placeholder: (*placeholder).to_string(),
                    change_port: *change_port,
                    key_down_port: *key_down_port,
                    blur_port: None,
                    focus_port: None,
                    focus_on_mount: *focus_on_mount,
                    disabled_sink: None,
                },
                children: Vec::new(),
            },
            FlatStripeDocumentChildConfig::MappedLabelList(list) => lower_mapped_label_list(list),
            FlatStripeDocumentChildConfig::MappedButtonLabelRows {
                container_view_site,
                rows,
            } => HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: *container_view_site,
                    function_instance,
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: lower_mapped_button_label_rows(rows),
            },
        })
        .collect();

    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: config.root_view_site,
                function_instance,
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: config.stripe_view_site,
                    function_instance,
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: stripe_children,
            }],
        }),
    }
}

#[derive(Clone, Copy)]
struct MappedButtonLabelRowsConfig<'a> {
    function_instance: FunctionInstanceId,
    row_view_site: ViewSiteId,
    button_view_site: ViewSiteId,
    button_label: &'a str,
    button_press_ports: &'a [SourcePortId],
    label_view_site: ViewSiteId,
    label_sinks: &'a [SinkPortId],
}

fn lower_mapped_button_label_rows(config: &MappedButtonLabelRowsConfig<'_>) -> Vec<HostViewNode> {
    config
        .button_press_ports
        .iter()
        .zip(config.label_sinks.iter())
        .enumerate()
        .map(|(index, (press_port, label_sink))| {
            let mapped_item_identity = Some((index as u64) + 1);
            HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: config.row_view_site,
                    function_instance: Some(config.function_instance),
                    mapped_item_identity,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: config.button_view_site,
                            function_instance: Some(config.function_instance),
                            mapped_item_identity,
                        },
                        kind: HostViewKind::Button {
                            label: HostButtonLabel::Static(config.button_label.to_string()),
                            press_port: *press_port,
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: config.label_view_site,
                            function_instance: Some(config.function_instance),
                            mapped_item_identity,
                        },
                        kind: HostViewKind::Label { sink: *label_sink },
                        children: Vec::new(),
                    },
                ],
            }
        })
        .collect()
}

fn lower_column_document(
    function_instance_id: FunctionInstanceId,
    root_view_site: ViewSiteId,
    stripe_view_site: ViewSiteId,
    gap_px: u32,
    padding_px: Option<u32>,
    width: Option<HostWidth>,
    align_cross: Option<HostCrossAlign>,
    children: Vec<HostViewNode>,
) -> HostViewIr {
    let function_instance = Some(function_instance_id);
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: root_view_site,
                function_instance,
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: stripe_view_site,
                    function_instance,
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    gap_px,
                    padding_px,
                    width,
                    align_cross,
                ),
                children,
            }],
        }),
    }
}

fn lower_titled_column_document(
    function_instance_id: FunctionInstanceId,
    root_view_site: ViewSiteId,
    stripe_view_site: ViewSiteId,
    gap_px: u32,
    padding_px: Option<u32>,
    width: Option<HostWidth>,
    title_view_site: ViewSiteId,
    title_sink: SinkPortId,
    title_font_size_px: u32,
    mut body_children: Vec<HostViewNode>,
) -> HostViewIr {
    let function_instance = Some(function_instance_id);
    let mut stripe_children = vec![HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: title_view_site,
            function_instance,
            mapped_item_identity: None,
        },
        kind: styled_label(title_sink, Some(title_font_size_px), true, None),
        children: Vec::new(),
    }];
    stripe_children.append(&mut body_children);

    lower_column_document(
        function_instance_id,
        root_view_site,
        stripe_view_site,
        gap_px,
        padding_px,
        width,
        None,
        stripe_children,
    )
}

struct PlainButtonRowConfig<'a> {
    function_instance: FunctionInstanceId,
    row_view_site: ViewSiteId,
    button_view_site: ViewSiteId,
    buttons: &'a [PlainButtonRowButtonConfig<'a>],
}

struct PlainButtonRowButtonConfig<'a> {
    mapped_item_identity: u64,
    label: &'a str,
    press_port: SourcePortId,
}

fn lower_plain_button_row(config: &PlainButtonRowConfig<'_>) -> HostViewNode {
    let function_instance = Some(config.function_instance);
    HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: config.row_view_site,
            function_instance,
            mapped_item_identity: None,
        },
        kind: HostViewKind::Stripe,
        children: config
            .buttons
            .iter()
            .map(|button| HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: config.button_view_site,
                    function_instance,
                    mapped_item_identity: Some(button.mapped_item_identity),
                },
                kind: HostViewKind::Button {
                    label: HostButtonLabel::Static(button.label.to_string()),
                    press_port: button.press_port,
                    disabled_sink: None,
                },
                children: Vec::new(),
            })
            .collect(),
    }
}

enum MappedCheckboxRowKind {
    PlainStripe,
    StyledRow { gap_px: u32 },
}

struct MappedCheckboxRowsConfig<'a> {
    function_instance: FunctionInstanceId,
    row_kind: MappedCheckboxRowKind,
    row_view_site: ViewSiteId,
    checkbox_view_site: ViewSiteId,
    label_view_site: ViewSiteId,
    checkbox_sinks: &'a [SinkPortId],
    checkbox_ports: &'a [SourcePortId],
    label_sinks: &'a [SinkPortId],
    status_view_site: Option<ViewSiteId>,
    status_sinks: &'a [SinkPortId],
    action_button_view_site: Option<ViewSiteId>,
    action_button_label: Option<&'a str>,
    action_button_ports: &'a [SourcePortId],
}

enum CheckboxListDocumentChildConfig<'a> {
    Label {
        view_site: ViewSiteId,
        sink: SinkPortId,
    },
    PlainButtonRow(PlainButtonRowConfig<'a>),
}

#[derive(Clone, Copy)]
enum CheckboxListDocumentContainerKind {
    Stripe,
    StyledColumn {
        gap_px: u32,
        padding_px: Option<u32>,
        width: Option<HostWidth>,
        align_cross: Option<HostCrossAlign>,
    },
}

fn lower_checkbox_list_document_child(
    function_instance: FunctionInstanceId,
    child: &CheckboxListDocumentChildConfig<'_>,
) -> HostViewNode {
    match child {
        CheckboxListDocumentChildConfig::Label { view_site, sink } => HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: *view_site,
                function_instance: Some(function_instance),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Label { sink: *sink },
            children: Vec::new(),
        },
        CheckboxListDocumentChildConfig::PlainButtonRow(config) => lower_plain_button_row(config),
    }
}

fn lower_checkbox_list_document(
    function_instance: FunctionInstanceId,
    root_view_site: ViewSiteId,
    container_view_site: ViewSiteId,
    container_kind: CheckboxListDocumentContainerKind,
    prefix_children: &[CheckboxListDocumentChildConfig<'_>],
    rows_container_view_site: Option<ViewSiteId>,
    rows: &MappedCheckboxRowsConfig<'_>,
    suffix_children: &[CheckboxListDocumentChildConfig<'_>],
) -> HostViewIr {
    let function_instance_opt = Some(function_instance);
    let mut children: Vec<_> = prefix_children
        .iter()
        .map(|child| lower_checkbox_list_document_child(function_instance, child))
        .collect();
    let row_children = lower_mapped_checkbox_rows(rows);
    match rows_container_view_site {
        Some(view_site) => children.push(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site,
                function_instance: function_instance_opt,
                mapped_item_identity: None,
            },
            kind: HostViewKind::Stripe,
            children: row_children,
        }),
        None => children.extend(row_children),
    }
    children.extend(
        suffix_children
            .iter()
            .map(|child| lower_checkbox_list_document_child(function_instance, child)),
    );

    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: root_view_site,
                function_instance: function_instance_opt,
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: container_view_site,
                    function_instance: function_instance_opt,
                    mapped_item_identity: None,
                },
                kind: match container_kind {
                    CheckboxListDocumentContainerKind::Stripe => HostViewKind::Stripe,
                    CheckboxListDocumentContainerKind::StyledColumn {
                        gap_px,
                        padding_px,
                        width,
                        align_cross,
                    } => styled_stripe_layout(
                        HostStripeDirection::Column,
                        gap_px,
                        padding_px,
                        width,
                        align_cross,
                    ),
                },
                children,
            }],
        }),
    }
}

fn lower_mapped_checkbox_rows(config: &MappedCheckboxRowsConfig<'_>) -> Vec<HostViewNode> {
    let function_instance = Some(config.function_instance);
    config
        .checkbox_sinks
        .iter()
        .zip(config.checkbox_ports.iter())
        .zip(config.label_sinks.iter())
        .enumerate()
        .map(|(index, ((checked_sink, click_port), label_sink))| {
            let mapped_item_identity = Some((index as u64) + 1);
            let mut children = vec![
                HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: config.checkbox_view_site,
                        function_instance,
                        mapped_item_identity,
                    },
                    kind: HostViewKind::Checkbox {
                        checked_sink: *checked_sink,
                        click_port: *click_port,
                    },
                    children: Vec::new(),
                },
                HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: config.label_view_site,
                        function_instance,
                        mapped_item_identity,
                    },
                    kind: HostViewKind::Label { sink: *label_sink },
                    children: Vec::new(),
                },
            ];
            if let Some(status_view_site) = config.status_view_site {
                let status_sink = config.status_sinks[index];
                children.push(HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: status_view_site,
                        function_instance,
                        mapped_item_identity,
                    },
                    kind: HostViewKind::Label { sink: status_sink },
                    children: Vec::new(),
                });
            }
            if let Some(action_button_view_site) = config.action_button_view_site {
                let action_button_label = config
                    .action_button_label
                    .expect("mapped checkbox rows action button label");
                let action_button_port = config.action_button_ports[index];
                children.push(HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: action_button_view_site,
                        function_instance,
                        mapped_item_identity,
                    },
                    kind: styled_button(
                        action_button_label,
                        action_button_port,
                        None,
                        None,
                        None,
                        false,
                        None,
                        None,
                        None,
                        None,
                        None,
                    ),
                    children: Vec::new(),
                });
            }

            HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: config.row_view_site,
                    function_instance,
                    mapped_item_identity,
                },
                kind: match config.row_kind {
                    MappedCheckboxRowKind::PlainStripe => HostViewKind::Stripe,
                    MappedCheckboxRowKind::StyledRow { gap_px } => {
                        styled_stripe_layout(HostStripeDirection::Row, gap_px, None, None, None)
                    }
                },
                children,
            }
        })
        .collect()
}

fn extract_document_root(
    expression: &StaticSpannedExpression,
) -> Result<&StaticSpannedExpression, String> {
    match &expression.node {
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Document", "new"]) =>
        {
            find_named_argument(arguments, "root")
                .ok_or_else(|| "Document/new requires `root`".to_string())
        }
        _ => Err("counter subset requires Document/new(root: ...)".to_string()),
    }
}

fn find_named_argument<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<
        boon::parser::static_expression::Argument,
    >],
    name: &str,
) -> Option<&'a StaticSpannedExpression> {
    arguments.iter().find_map(|argument| {
        (argument.node.name.as_str() == name)
            .then(|| argument.node.value.as_ref())
            .flatten()
    })
}

fn path_matches(path: &[boon::parser::StrSlice], expected: &[&str]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected.iter())
            .all(|(actual, expected)| actual.as_str() == *expected)
}

fn extract_press_link(expression: &StaticSpannedExpression) -> Result<SourcePortId, String> {
    let StaticExpression::Object(object) = &expression.node else {
        return Err("button element must be object".to_string());
    };
    let event_var = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "event")
        .ok_or_else(|| "button element requires event object".to_string())?;
    let StaticExpression::Object(event_object) = &event_var.node.value.node else {
        return Err("button event must be object".to_string());
    };
    let press_var = event_object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "press")
        .ok_or_else(|| "button event object requires press".to_string())?;
    match &press_var.node.value.node {
        StaticExpression::Link => Ok(SourcePortId(1)),
        _ => Err("button press must be LINK".to_string()),
    }
}

fn extract_button_label(expression: &StaticSpannedExpression) -> Result<String, String> {
    match &expression.node {
        StaticExpression::TextLiteral { parts, .. } => {
            if parts.len() != 1 {
                return Err("counter subset requires static button text".to_string());
            }
            match &parts[0] {
                boon::parser::static_expression::TextPart::Text(text) => {
                    Ok(text.as_str().to_string())
                }
                _ => Err("counter subset requires static button text".to_string()),
            }
        }
        _ => Err("counter subset requires text label".to_string()),
    }
}

fn extract_integer_literal(expression: &StaticSpannedExpression) -> Result<i64, String> {
    let StaticExpression::Literal(boon::parser::static_expression::Literal::Number(number)) =
        &expression.node
    else {
        return Err("counter subset requires integer numeric literals".to_string());
    };
    if number.fract() != 0.0 {
        return Err("counter subset requires integer numeric literals".to_string());
    }
    Ok(*number as i64)
}

fn extract_number_literal(expression: &StaticSpannedExpression) -> Result<f64, String> {
    let StaticExpression::Literal(boon::parser::static_expression::Literal::Number(number)) =
        &expression.node
    else {
        return Err("subset requires numeric literal".to_string());
    };
    Ok(*number)
}

fn extract_static_kernel_value(
    expression: &StaticSpannedExpression,
) -> Result<KernelValue, String> {
    match &expression.node {
        StaticExpression::Literal(boon::parser::static_expression::Literal::Number(number)) => {
            Ok(KernelValue::from(*number))
        }
        StaticExpression::Literal(boon::parser::static_expression::Literal::Text(text))
        | StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(text)) => {
            Ok(KernelValue::from(text.as_str()))
        }
        StaticExpression::TextLiteral { parts, .. } => {
            let mut out = String::new();
            for part in parts {
                match part {
                    boon::parser::static_expression::TextPart::Text(text) => {
                        out.push_str(text.as_str());
                    }
                    boon::parser::static_expression::TextPart::Interpolation { .. } => {
                        return Err("static subset does not support interpolated text".to_string());
                    }
                }
            }
            Ok(KernelValue::from(out))
        }
        _ => Err("static subset requires literal/text root".to_string()),
    }
}

fn extract_event_press_port(expression: &StaticSpannedExpression) -> Result<SourcePortId, String> {
    match &expression.node {
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) if parts.len() >= 3
            && parts[0].as_str() == "increment_button"
            && parts[parts.len() - 2].as_str() == "event"
            && parts[parts.len() - 1].as_str() == "press" =>
        {
            Ok(SourcePortId(1))
        }
        _ => Err("counter subset requires increment_button.event.press trigger".to_string()),
    }
}

fn ensure_alias_name(expression: &StaticSpannedExpression, expected: &str) -> Result<(), String> {
    match &expression.node {
        StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
            parts,
            ..
        }) if parts.len() == 1 && parts[0].as_str() == expected => Ok(()),
        _ => Err(format!("counter subset expected alias `{expected}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells_preview::try_lower_cells_program;
    use crate::ir::PersistPolicy;

    #[test]
    fn lowers_real_counter_example() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let program = try_lower_counter(source).expect("counter should lower");
        assert_eq!(program.initial_value, 0);
        assert_eq!(program.increment_delta, 1);
        assert_eq!(program.press_port, SourcePortId(1));
        assert_eq!(program.counter_sink, SinkPortId(1));
        assert_eq!(program.ir.nodes.len(), 7);
        assert_eq!(program.ir.persistence.len(), 1);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_counter_hold_example() {
        let source =
            include_str!("../../../playground/frontend/src/examples/counter_hold/counter_hold.bn");
        let program = try_lower_counter(source).expect("counter_hold should lower");
        assert_eq!(program.initial_value, 0);
        assert_eq!(program.increment_delta, 1);
        assert_eq!(program.press_port, SourcePortId(1));
        assert_eq!(program.counter_sink, SinkPortId(1));
        assert_eq!(program.ir.nodes.len(), 7);
        assert_eq!(program.ir.persistence.len(), 1);
        let policy = program.ir.persist_policy(NodeId(6));
        let PersistPolicy::Durable {
            root_key,
            local_slot,
            persist_kind,
        } = policy
        else {
            panic!("counter_hold should carry durable hold metadata");
        };
        assert_ne!(root_key.as_u128(), 0);
        assert_eq!(local_slot, 0);
        assert_eq!(persist_kind, PersistKind::Hold);
    }

    #[test]
    fn lowers_real_complex_counter_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/complex_counter/complex_counter.bn"
        );
        let program = try_lower_complex_counter(source).expect("complex_counter should lower");
        assert_eq!(program.initial_value, 0);
        assert_eq!(program.decrement_port, SourcePortId(10));
        assert_eq!(program.increment_port, SourcePortId(11));
        assert_eq!(program.decrement_hovered_cell, MirrorCellId(20));
        assert_eq!(program.increment_hovered_cell, MirrorCellId(21));
        assert_eq!(program.counter_sink, SinkPortId(10));
        assert_eq!(program.decrement_hovered_sink, SinkPortId(11));
        assert_eq!(program.increment_hovered_sink, SinkPortId(12));
        assert_eq!(program.ir.nodes.len(), 16);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_interval_example() {
        let source = include_str!("../../../playground/frontend/src/examples/interval/interval.bn");
        let program = try_lower_interval(source).expect("interval should lower");
        assert_eq!(program.tick_port, SourcePortId(1980));
        assert_eq!(program.value_sink, SinkPortId(1980));
        assert_eq!(program.interval_ms, 1000);
        assert_eq!(program.ir.nodes.len(), 5);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_interval_hold_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/interval_hold/interval_hold.bn"
        );
        let program = try_lower_interval_hold(source).expect("interval_hold should lower");
        assert_eq!(program.tick_port, SourcePortId(1981));
        assert_eq!(program.value_sink, SinkPortId(1981));
        assert_eq!(program.interval_ms, 1000);
        assert_eq!(program.ir.nodes.len(), 8);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_fibonacci_example() {
        let source =
            include_str!("../../../playground/frontend/src/examples/fibonacci/fibonacci.bn");
        let program = try_lower_fibonacci(source).expect("fibonacci should lower");
        assert_eq!(
            program.sink_values.get(&SinkPortId(1985)),
            Some(&KernelValue::from("10. Fibonacci number is 55"))
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_layers_example() {
        let source = include_str!("../../../playground/frontend/src/examples/layers/layers.bn");
        let program = try_lower_layers(source).expect("layers should lower");
        assert_eq!(
            program.sink_values.get(&SinkPortId(1986)),
            Some(&KernelValue::from("Red Card"))
        );
        assert_eq!(
            program.sink_values.get(&SinkPortId(1988)),
            Some(&KernelValue::from("Blue Card"))
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_pages_example() {
        let source = include_str!("../../../playground/frontend/src/examples/pages/pages.bn");
        let program = try_lower_pages(source).expect("pages should lower");
        assert_eq!(
            program.nav_press_ports,
            [SourcePortId(1989), SourcePortId(1990), SourcePortId(1991)]
        );
        assert_eq!(program.current_page_sink, SinkPortId(1994));
        assert_eq!(program.title_sink, SinkPortId(1989));
        assert_eq!(program.description_sink, SinkPortId(1990));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_latest_example() {
        let source = include_str!("../../../playground/frontend/src/examples/latest/latest.bn");
        let program = try_lower_latest(source).expect("latest should lower");
        assert_eq!(
            program.send_press_ports,
            [SourcePortId(1994), SourcePortId(1995)]
        );
        assert_eq!(program.value_sink, SinkPortId(1994));
        assert_eq!(program.sum_sink, SinkPortId(1995));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_text_interpolation_update_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/text_interpolation_update/text_interpolation_update.bn"
        );
        let program = try_lower_text_interpolation_update(source)
            .expect("text_interpolation_update should lower");
        assert_eq!(program.toggle_press_port, SourcePortId(1996));
        assert_eq!(program.button_label_sink, SinkPortId(1996));
        assert_eq!(program.while_sink, SinkPortId(1998));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_then_example() {
        let source = include_str!("../../../playground/frontend/src/examples/then/then.bn");
        let program = try_lower_then(source).expect("then should lower");
        assert_eq!(program.input_a_tick_port, SourcePortId(2010));
        assert_eq!(program.input_b_tick_port, SourcePortId(2011));
        assert_eq!(program.addition_press_port, SourcePortId(2012));
        assert_eq!(program.ir.persistence.len(), 2);
        assert_eq!(program.result_sink, SinkPortId(2012));
        let PersistPolicy::Durable {
            persist_kind,
            local_slot,
            ..
        } = program.ir.persist_policy(NodeId(2105))
        else {
            panic!("then input_a should carry durable hold metadata");
        };
        assert_eq!(persist_kind, PersistKind::Hold);
        assert_eq!(local_slot, 0);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_when_example() {
        let source = include_str!("../../../playground/frontend/src/examples/when/when.bn");
        let program = try_lower_when(source).expect("when should lower");
        assert_eq!(program.input_a_tick_port, SourcePortId(2013));
        assert_eq!(program.input_b_tick_port, SourcePortId(2014));
        assert_eq!(program.addition_press_port, SourcePortId(2015));
        assert_eq!(program.subtraction_press_port, SourcePortId(2016));
        assert_eq!(program.ir.persistence.len(), 2);
        assert_eq!(program.result_sink, SinkPortId(2015));
        let PersistPolicy::Durable {
            persist_kind,
            local_slot,
            ..
        } = program.ir.persist_policy(NodeId(2205))
        else {
            panic!("when input_a should carry durable hold metadata");
        };
        assert_eq!(persist_kind, PersistKind::Hold);
        assert_eq!(local_slot, 0);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_while_example() {
        let source = include_str!("../../../playground/frontend/src/examples/while/while.bn");
        let program = try_lower_while(source).expect("while should lower");
        assert_eq!(program.input_a_tick_port, SourcePortId(2016));
        assert_eq!(program.input_b_tick_port, SourcePortId(2017));
        assert_eq!(program.addition_press_port, SourcePortId(2019));
        assert_eq!(program.subtraction_press_port, SourcePortId(2020));
        assert!(program.ir.persistence.is_empty());
        assert_eq!(program.result_sink, SinkPortId(2018));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn generic_lower_program_covers_control_flow_examples() {
        let complex_counter = include_str!(
            "../../../playground/frontend/src/examples/complex_counter/complex_counter.bn"
        );
        let fibonacci =
            include_str!("../../../playground/frontend/src/examples/fibonacci/fibonacci.bn");
        let layers = include_str!("../../../playground/frontend/src/examples/layers/layers.bn");
        let pages = include_str!("../../../playground/frontend/src/examples/pages/pages.bn");
        let interval =
            include_str!("../../../playground/frontend/src/examples/interval/interval.bn");
        let interval_hold = include_str!(
            "../../../playground/frontend/src/examples/interval_hold/interval_hold.bn"
        );
        let latest = include_str!("../../../playground/frontend/src/examples/latest/latest.bn");
        let text_interpolation_update = include_str!(
            "../../../playground/frontend/src/examples/text_interpolation_update/text_interpolation_update.bn"
        );
        let button_hover_to_click_test = include_str!(
            "../../../playground/frontend/src/examples/button_hover_to_click_test/button_hover_to_click_test.bn"
        );
        let button_hover_test = include_str!(
            "../../../playground/frontend/src/examples/button_hover_test/button_hover_test.bn"
        );
        let filter_checkbox_bug = include_str!(
            "../../../playground/frontend/src/examples/filter_checkbox_bug/filter_checkbox_bug.bn"
        );
        let checkbox_test = include_str!(
            "../../../playground/frontend/src/examples/checkbox_test/checkbox_test.bn"
        );
        let temperature_converter = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let flight_booker = include_str!(
            "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
        );
        let timer = include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
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
        let chained_list_remove_bug = include_str!(
            "../../../playground/frontend/src/examples/chained_list_remove_bug/chained_list_remove_bug.bn"
        );
        let crud = include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let list_retain_remove = include_str!(
            "../../../playground/frontend/src/examples/list_retain_remove/list_retain_remove.bn"
        );
        let shopping_list = include_str!(
            "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
        );
        let list_retain_reactive = include_str!(
            "../../../playground/frontend/src/examples/list_retain_reactive/list_retain_reactive.bn"
        );
        let then = include_str!("../../../playground/frontend/src/examples/then/then.bn");
        let when = include_str!("../../../playground/frontend/src/examples/when/when.bn");
        let while_source = include_str!("../../../playground/frontend/src/examples/while/while.bn");
        let while_function_call = include_str!(
            "../../../playground/frontend/src/examples/while_function_call/while_function_call.bn"
        );
        let switch_hold_test = include_str!(
            "../../../playground/frontend/src/examples/switch_hold_test/switch_hold_test.bn"
        );
        let circle_drawer = include_str!(
            "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
        );
        let cells = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let cells_dynamic = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );

        assert!(matches!(
            lower_program(complex_counter).expect("complex_counter lowers generically"),
            LoweredProgram::ComplexCounter(_)
        ));
        assert!(matches!(
            lower_program(fibonacci).expect("fibonacci lowers generically"),
            LoweredProgram::Fibonacci(_)
        ));
        assert!(matches!(
            lower_program(layers).expect("layers lowers generically"),
            LoweredProgram::Layers(_)
        ));
        assert!(matches!(
            lower_program(pages).expect("pages lowers generically"),
            LoweredProgram::Pages(_)
        ));
        assert!(matches!(
            lower_program(interval).expect("interval lowers generically"),
            LoweredProgram::Interval(_)
        ));
        assert!(matches!(
            lower_program(interval_hold).expect("interval_hold lowers generically"),
            LoweredProgram::IntervalHold(_)
        ));
        assert!(matches!(
            lower_program(latest).expect("latest lowers generically"),
            LoweredProgram::Latest(_)
        ));
        assert!(matches!(
            lower_program(text_interpolation_update)
                .expect("text_interpolation_update lowers generically"),
            LoweredProgram::TextInterpolationUpdate(_)
        ));
        assert!(matches!(
            lower_program(button_hover_to_click_test)
                .expect("button_hover_to_click_test lowers generically"),
            LoweredProgram::ButtonHoverToClickTest(_)
        ));
        assert!(matches!(
            lower_program(button_hover_test).expect("button_hover_test lowers generically"),
            LoweredProgram::ButtonHoverTest(_)
        ));
        assert!(matches!(
            lower_program(filter_checkbox_bug).expect("filter_checkbox_bug lowers generically"),
            LoweredProgram::FilterCheckboxBug(_)
        ));
        assert!(matches!(
            lower_program(checkbox_test).expect("checkbox_test lowers generically"),
            LoweredProgram::CheckboxTest(_)
        ));
        assert!(matches!(
            lower_program(temperature_converter).expect("temperature_converter lowers generically"),
            LoweredProgram::TemperatureConverter(_)
        ));
        assert!(matches!(
            lower_program(flight_booker).expect("flight_booker lowers generically"),
            LoweredProgram::FlightBooker(_)
        ));
        assert!(matches!(
            lower_program(timer).expect("timer lowers generically"),
            LoweredProgram::Timer(_)
        ));
        assert!(matches!(
            lower_program(list_map_external_dep).expect("list_map_external_dep lowers generically"),
            LoweredProgram::ListMapExternalDep(_)
        ));
        assert!(matches!(
            lower_program(list_map_block).expect("list_map_block lowers generically"),
            LoweredProgram::ListMapBlock(_)
        ));
        assert!(matches!(
            lower_program(list_retain_count).expect("list_retain_count lowers generically"),
            LoweredProgram::ListRetainCount(_)
        ));
        assert!(matches!(
            lower_program(list_object_state).expect("list_object_state lowers generically"),
            LoweredProgram::ListObjectState(_)
        ));
        assert!(matches!(
            lower_program(chained_list_remove_bug)
                .expect("chained_list_remove_bug lowers generically"),
            LoweredProgram::ChainedListRemoveBug(_)
        ));
        assert!(matches!(
            lower_program(crud).expect("crud lowers generically"),
            LoweredProgram::Crud(_)
        ));
        assert!(matches!(
            lower_program(list_retain_remove).expect("list_retain_remove lowers generically"),
            LoweredProgram::ListRetainRemove(_)
        ));
        assert!(matches!(
            lower_program(shopping_list).expect("shopping_list lowers generically"),
            LoweredProgram::ShoppingList(_)
        ));
        assert!(matches!(
            lower_program(list_retain_reactive).expect("list_retain_reactive lowers generically"),
            LoweredProgram::ListRetainReactive(_)
        ));
        assert!(matches!(
            lower_program(then).expect("then lowers generically"),
            LoweredProgram::Then(_)
        ));
        assert!(matches!(
            lower_program(when).expect("when lowers generically"),
            LoweredProgram::When(_)
        ));
        assert!(matches!(
            lower_program(while_source).expect("while lowers generically"),
            LoweredProgram::While(_)
        ));
        assert!(matches!(
            lower_program(while_function_call).expect("while_function_call lowers generically"),
            LoweredProgram::WhileFunctionCall(_)
        ));
        assert!(matches!(
            lower_program(switch_hold_test).expect("switch_hold_test lowers generically"),
            LoweredProgram::SwitchHoldTest(_)
        ));
        assert!(matches!(
            lower_program(circle_drawer).expect("circle_drawer lowers generically"),
            LoweredProgram::CircleDrawer(_)
        ));
        assert!(matches!(
            lower_program(cells).expect("cells lowers generically"),
            LoweredProgram::Cells(_)
        ));
        assert!(matches!(
            lower_program(cells_dynamic).expect("cells_dynamic lowers generically"),
            LoweredProgram::Cells(_)
        ));
    }

    #[test]
    fn generic_lower_program_and_view_accept_minimal_static_snippet() {
        let source = r#"
document: Document/new(root: 123)
"#;

        assert!(matches!(
            lower_program(source).expect("minimal static snippet lowers generically"),
            LoweredProgram::StaticDocument(_)
        ));
        assert!(
            lower_view(source)
                .expect("minimal static snippet lowers into host view")
                .root
                .is_some()
        );
    }

    #[test]
    fn generic_lower_view_accepts_signal_pipeline_document_via_root_sink_binding() {
        let source = r#"
document:
    Duration[seconds: 1]
    |> Timer/interval()
    |> THEN { 1 }
    |> Math/sum()
    |> Document/new()
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view_with_root_binding(
            &expressions,
            &[("document", SinkPortId(7001))],
            &[],
            ViewSiteId(7001),
            FunctionInstanceId(7001),
            Some("document"),
        )
        .expect("signal pipeline document lowers via root sink binding");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::Label { sink } if sink == SinkPortId(7001)
        ));
    }

    #[test]
    fn generic_lower_view_accepts_block_scoped_bindings() {
        let source = r#"
value: TEXT { Hello }
document: Document/new(root: BLOCK {
    local_value: value
    local_value
})
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[],
            &[],
            ViewSiteId(7000),
            FunctionInstanceId(7000),
        )
        .expect("block-scoped view snippet lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::StaticLabel { ref text } if text == "Hello"
        ));
    }

    #[test]
    fn generic_lower_view_accepts_link_setter_pipes() {
        let source = r#"
store: [nav: [home: LINK]]

document: Document/new(root: BLOCK {
    nav_button: Element/button(
        element: [event: [press: LINK]]
        label: TEXT { Home }
    ) |> LINK { store.nav.home }

    nav_button
})
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[],
            &[("store.nav.home", SourcePortId(7100))],
            ViewSiteId(7100),
            FunctionInstanceId(7100),
        )
        .expect("link-setter view snippet lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::Button {
                ref label,
                press_port,
                disabled_sink: None,
            } if *label == HostButtonLabel::Static("Home".to_string())
                && press_port == SourcePortId(7100)
        ));
    }

    #[test]
    fn generic_lower_view_accepts_link_setter_pipes_with_passed_alias() {
        let source = r#"
store: [nav: [home: LINK]]

document: Document/new(root: root(PASS: store))

FUNCTION root() {
    Element/button(
        element: [event: [press: LINK]]
        label: TEXT { Home }
    ) |> LINK { PASSED.nav.home }
}
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[],
            &[("store.nav.home", SourcePortId(7101))],
            ViewSiteId(7101),
            FunctionInstanceId(7101),
        )
        .expect("passed link-setter view snippet lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::Button {
                ref label,
                press_port,
                disabled_sink: None,
            } if *label == HostButtonLabel::Static("Home".to_string())
                && press_port == SourcePortId(7101)
        ));
    }

    #[test]
    fn generic_lower_view_accepts_nested_static_alias_button_labels() {
        let source = r#"
store: [
    btn_a: [
        elements: [button: LINK]
        name: TEXT { A }
    ]
]

document: Document/new(root:
    Element/button(
        element: [event: [press: LINK]]
        label: TEXT { Button {store.btn_a.name} }
    ) |> LINK { store.btn_a.elements.button }
)
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[],
            &[("store.btn_a.elements.button", SourcePortId(7102))],
            ViewSiteId(7102),
            FunctionInstanceId(7102),
        )
        .expect("nested static alias button label lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::Button {
                ref label,
                press_port,
                disabled_sink: None,
            } if *label == HostButtonLabel::Static("Button A".to_string())
                && press_port == SourcePortId(7102)
        ));
    }

    #[test]
    fn generic_lower_view_accepts_svg_circle_canvas() {
        let source = r#"
store: [
    elements: [canvas: LINK]
    circles: LIST {}
]

document: Document/new(root:
    Element/svg(
        element: [event: [click: LINK]]
        style: [
            width: 460
            height: 300
            background: TEXT { rgba(255,255,255,0.1) }
        ]
        children: store.circles |> List/map(item, new:
            Element/svg_circle(
                element: []
                cx: item.x
                cy: item.y
                r: 20
                style: [
                    fill: TEXT { #3498db }
                    stroke: TEXT { #2c3e50 }
                    stroke_width: 2
                ]
            )
        )
    ) |> LINK { store.elements.canvas }
)
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[("store.circles", SinkPortId(7104))],
            &[("store.elements.canvas", SourcePortId(7103))],
            ViewSiteId(7103),
            FunctionInstanceId(7103),
        )
        .expect("svg canvas view snippet lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::AbsoluteCanvas {
                click_port,
                width_px: 460,
                height_px: 300,
                ref background,
            } if click_port == SourcePortId(7103) && background == "rgba(255,255,255,0.1)"
        ));
        let circles = child.children.first().expect("circle list child");
        assert!(matches!(
            circles.kind,
            HostViewKind::PositionedCircleList {
                circles_sink,
                radius_px: 20,
                ref fill,
                ref stroke,
                stroke_width_px: 2,
            } if circles_sink == SinkPortId(7104)
                && fill == "#3498db"
                && stroke == "#2c3e50"
        ));
    }

    #[test]
    fn generic_lower_view_accepts_sink_bound_local_text_input_alias() {
        let source = r#"
store: [elements: [input: LINK]]

document: Document/new(root: BLOCK {
    local_value: TEXT { shadowed }

    Element/text_input(
        element: [event: [change: LINK]]
        style: [width: 120]
        label: Hidden[text: TEXT { Celsius }]
        text: local_value
        placeholder: [text: TEXT { Celsius }]
        focus: False
    ) |> LINK { store.elements.input }
})
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[("local_value", SinkPortId(7105))],
            &[("store.elements.input", SourcePortId(7106))],
            ViewSiteId(7105),
            FunctionInstanceId(7105),
        )
        .expect("text input snippet lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::StyledTextInput {
                value_sink,
                ref placeholder,
                change_port,
                focus_on_mount: false,
                width: Some(HostWidth::Px(120)),
                ..
            } if value_sink == SinkPortId(7105)
                && placeholder == "Celsius"
                && change_port == SourcePortId(7106)
        ));
    }

    #[test]
    fn generic_lower_view_accepts_styled_slider() {
        let source = r#"
store: [
    elements: [duration_slider: LINK]
    max_duration: 15
]

document: Document/new(root:
    Element/slider(
        element: [event: [change: LINK]]
        style: [width: 200]
        label: Hidden[text: TEXT { Duration }]
        value: store.max_duration
        min: 1
        max: 30
        step: 0.1
    ) |> LINK { store.elements.duration_slider }
)
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[("store.max_duration", SinkPortId(7107))],
            &[("store.elements.duration_slider", SourcePortId(7108))],
            ViewSiteId(7107),
            FunctionInstanceId(7107),
        )
        .expect("slider snippet lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::StyledSlider {
                value_sink,
                input_port,
                ref min,
                ref max,
                ref step,
                width: Some(HostWidth::Px(200)),
                ..
            } if value_sink == SinkPortId(7107)
                && input_port == SourcePortId(7108)
                && min == "1"
                && max == "30"
                && step == "0.1"
        ));
    }

    #[test]
    fn generic_lower_view_accepts_link_backed_select_and_disabled_control_fallbacks() {
        let source = r#"
store: [
    elements: [
        flight_select: LINK
        return_input: LINK
        book_button: LINK
    ]
    return_date: TEXT { 2026-03-03 }
    booked: TEXT { Booked }
]

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Column
        gap: 12
        style: [width: 300]
        items: LIST {
            Element/select(
                element: [event: [change: LINK]]
                style: [width: Fill]
                label: Hidden[text: TEXT { Flight type }]
                options: LIST {
                    [value: TEXT { one-way }, label: TEXT { One-way flight }]
                    [value: TEXT { return }, label: TEXT { Return flight }]
                }
                selected: TEXT { one-way }
            ) |> LINK { store.elements.flight_select }

            Element/text_input(
                element: [event: [change: LINK]]
                style: [width: Fill, disabled: False]
                label: Hidden[text: TEXT { Return date }]
                text: store.return_date
                placeholder: [text: TEXT { YYYY-MM-DD }]
                focus: False
            ) |> LINK { store.elements.return_input }

            Element/button(
                element: [event: [press: LINK]]
                style: [width: Fill, disabled: False]
                label: TEXT { Book }
            ) |> LINK { store.elements.book_button }

            Element/label(
                element: []
                style: []
                label: store.booked
            )
        }
    )
)
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[
                ("store.elements.flight_select.selected", SinkPortId(7109)),
                ("store.return_date", SinkPortId(7110)),
                ("store.elements.return_input.disabled", SinkPortId(7111)),
                ("store.elements.book_button.disabled", SinkPortId(7112)),
                ("store.booked", SinkPortId(7113)),
            ],
            &[
                ("store.elements.flight_select", SourcePortId(7109)),
                ("store.elements.return_input", SourcePortId(7110)),
                ("store.elements.book_button", SourcePortId(7111)),
            ],
            ViewSiteId(7109),
            FunctionInstanceId(7109),
        )
        .expect("select and disabled control snippet lowers");
        let root = host_view.root.expect("document root");
        let stripe = root.children.first().expect("stripe root child");

        assert!(matches!(
            stripe.children[0].kind,
            HostViewKind::StyledSelect {
                selected_sink,
                change_port,
                ref options,
                disabled_sink: None,
                width: Some(HostWidth::Fill),
            } if selected_sink == SinkPortId(7109)
                && change_port == SourcePortId(7109)
                && options.len() == 2
                && options[0].value == "one-way"
                && options[1].label == "Return flight"
        ));
        assert!(matches!(
            stripe.children[1].kind,
            HostViewKind::StyledTextInput {
                value_sink,
                ref placeholder,
                change_port,
                disabled_sink: Some(disabled_sink),
                width: Some(HostWidth::Fill),
                ..
            } if value_sink == SinkPortId(7110)
                && placeholder == "YYYY-MM-DD"
                && change_port == SourcePortId(7110)
                && disabled_sink == SinkPortId(7111)
        ));
        assert!(matches!(
            stripe.children[2].kind,
            HostViewKind::StyledButton {
                ref label,
                press_port,
                disabled_sink: Some(disabled_sink),
                width: Some(HostWidth::Fill),
                ..
            } if *label == HostButtonLabel::Static("Book".to_string())
                && press_port == SourcePortId(7111)
                && disabled_sink == SinkPortId(7112)
        ));
    }

    #[test]
    fn generic_lower_view_accepts_link_backed_active_style_fallbacks() {
        let source = r#"
store: [nav: [home: LINK]]
current_route: TEXT { / }

document: Document/new(root:
    nav_button(label: TEXT { Home }, route: TEXT { / })
    |> LINK { store.nav.home }
)

FUNCTION nav_button(label, route) {
    BLOCK {
        is_active: current_route == route

        Element/button(
            element: [event: [press: LINK]]
            style: [
                background: [
                    color: is_active |> WHEN {
                        True => Oklch[lightness: 0.3]
                        False => Oklch[lightness: 0.2]
                    }
                ]
            ]
            label: label
        )
    }
}
"#;

        let expressions = parse_static_expressions(source).expect("snippet parses");
        let host_view = lower_generic_host_view(
            &expressions,
            &[("store.nav.home.active", SinkPortId(7114))],
            &[("store.nav.home", SourcePortId(7114))],
            ViewSiteId(7114),
            FunctionInstanceId(7114),
        )
        .expect("active style fallback lowers");
        let root = host_view.root.expect("document root");
        let child = root.children.first().expect("document child");
        assert!(matches!(
            child.kind,
            HostViewKind::StyledButton {
                ref label,
                press_port,
                disabled_sink: None,
                background_sink: Some(background_sink),
                ref background,
                ref active_background,
                ..
            } if *label == HostButtonLabel::Static("Home".to_string())
                && press_port == SourcePortId(7114)
                && background_sink == SinkPortId(7114)
                && background.as_deref() == Some("oklch(0.2 0 0)")
                && active_background.as_deref() == Some("oklch(0.3 0 0)")
        ));
    }

    #[test]
    fn generic_lower_program_reports_explicit_subset_diagnostics() {
        let source = r#"
value: 123
"#;

        let error = lower_program(source).expect_err("unsupported snippet should fail explicitly");
        assert!(error.contains("unsupported generic lowering surface"));
        assert!(error.contains("single_action_accumulator_document:"));
        assert!(error.contains("dual_action_accumulator_document:"));
        assert!(error.contains("editable_filterable_list_document:"));
        assert!(error.contains("summed_interval_signal_document:"));
        assert!(error.contains("held_interval_signal_document:"));
        assert!(error.contains("sequence_message_display:"));
        assert!(error.contains("static_stack_display:"));
        assert!(error.contains("nav_selection_document:"));
        assert!(error.contains("latest_signal_document:"));
        assert!(error.contains("toggle_templated_label_document:"));
        assert!(error.contains("multi_button_activation_document:"));
        assert!(error.contains("multi_button_hover_document:"));
        assert!(error.contains("filterable_checkbox_list_document:"));
        assert!(error.contains("independent_checkbox_list_document:"));
        assert!(error.contains("bidirectional_conversion_form_document:"));
        assert!(error.contains("selectable_dual_date_form_document:"));
        assert!(error.contains("resettable_timed_progress_document:"));
        assert!(error.contains("external_mode_mapped_items_document:"));
        assert!(error.contains("retained_toggle_filter_list_document:"));
        assert!(error.contains("dual_mapped_label_stripes_document:"));
        assert!(error.contains("counted_filtered_append_list_document:"));
        assert!(error.contains("independent_object_counters_document:"));
        assert!(error.contains("removable_checkbox_list_document:"));
        assert!(error.contains("selectable_record_column_document:"));
        assert!(error.contains("removable_append_list_document:"));
        assert!(error.contains("clearable_append_list_document:"));
        assert!(error.contains("timed_addition_hold_document:"));
        assert!(error.contains("timed_operation_hold_document:"));
        assert!(error.contains("timed_operation_stream_document:"));
        assert!(error.contains("toggle_branch_document:"));
        assert!(error.contains("switched_hold_items_document:"));
        assert!(error.contains("canvas_history_document:"));
        assert!(error.contains("persistent_indexed_text_grid_document:"));
        assert!(error.contains("static_document_display:"));
    }

    #[test]
    fn generic_lower_view_lowers_todo_host_view_generically() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");

        let host_view = lower_view(source).expect("todo_mvc should lower into generic host view");
        let root = host_view
            .root
            .expect("todo_mvc host view should have a root");

        fn contains_node(
            node: &HostViewNode,
            view_site: ViewSiteId,
            mapped_item_identity: Option<u64>,
        ) -> bool {
            (node.retained_key.view_site == view_site
                && node.retained_key.mapped_item_identity == mapped_item_identity)
                || node
                    .children
                    .iter()
                    .any(|child| contains_node(child, view_site, mapped_item_identity))
        }

        assert_eq!(root.retained_key.view_site, ViewSiteId(200));
        assert!(contains_node(&root, ViewSiteId(300), Some(1)));
        assert!(contains_node(&root, ViewSiteId(300), Some(2)));
        assert!(contains_node(&root, ViewSiteId(2141), None));
    }

    #[test]
    fn generic_lower_view_lowers_crud_host_view_generically() {
        let source = include_str!("../../../playground/frontend/src/examples/crud/crud.bn");

        let host_view = lower_view(source).expect("crud should lower into generic host view");
        let root = host_view.root.expect("crud host view should have a root");

        assert_eq!(root.retained_key.view_site, ViewSiteId(1700));
        assert!(
            root.children
                .iter()
                .any(|child| child.retained_key.view_site == ViewSiteId(1701))
        );
    }

    #[test]
    fn generic_lower_view_lowers_cells_host_view_generically() {
        fn contains_node(
            node: &HostViewNode,
            view_site: ViewSiteId,
            mapped_item_identity: Option<u64>,
        ) -> bool {
            (node.retained_key.view_site == view_site
                && node.retained_key.mapped_item_identity == mapped_item_identity)
                || node
                    .children
                    .iter()
                    .any(|child| contains_node(child, view_site, mapped_item_identity))
        }

        let static_source =
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let dynamic_source = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );

        let static_host_view =
            lower_view(static_source).expect("cells should lower into generic host view");
        let dynamic_host_view =
            lower_view(dynamic_source).expect("cells_dynamic should lower into generic host view");

        let static_root = static_host_view
            .root
            .expect("cells host view should have a root");
        let dynamic_root = dynamic_host_view
            .root
            .expect("cells_dynamic host view should have a root");

        assert_eq!(static_root.retained_key.view_site, ViewSiteId(401));
        assert_eq!(dynamic_root.retained_key.view_site, ViewSiteId(401));
        assert!(contains_node(&static_root, ViewSiteId(432), Some(1)));
        assert!(contains_node(&static_root, ViewSiteId(434), Some(1_001)));
        assert!(contains_node(&dynamic_root, ViewSiteId(432), Some(100)));
        assert!(contains_node(&dynamic_root, ViewSiteId(434), Some(100_026)));
    }

    #[test]
    fn lowers_real_cells_examples_with_durable_override_metadata() {
        let cells = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let cells_dynamic = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );

        for (source, title) in [(cells, "Cells"), (cells_dynamic, "Cells Dynamic")] {
            let program = try_lower_cells_program(source).expect("cells example should lower");
            assert_eq!(program.title, title);
            assert_eq!(program.ir.persistence.len(), 1);
            let PersistPolicy::Durable {
                root_key,
                local_slot,
                persist_kind,
            } = program
                .ir
                .persist_policy(CellsProgram::OVERRIDES_LIST_HOLD_NODE)
            else {
                panic!("cells overrides should carry durable list-store metadata");
            };
            assert_ne!(root_key.as_u128(), 0);
            assert_eq!(local_slot, 0);
            assert_eq!(persist_kind, PersistKind::ListStore);
        }
    }

    #[test]
    fn lowers_real_list_retain_reactive_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_reactive/list_retain_reactive.bn"
        );
        let program =
            try_lower_list_retain_reactive(source).expect("list_retain_reactive should lower");
        assert_eq!(program.toggle_port, SourcePortId(30));
        assert_eq!(program.mode_sink, SinkPortId(30));
        assert_eq!(program.count_sink, SinkPortId(31));
        assert_eq!(program.items_list_sink, SinkPortId(38));
        assert_eq!(program.item_sinks[0], SinkPortId(32));
        assert_eq!(program.item_sinks[5], SinkPortId(37));
        assert_eq!(program.ir.persistence.len(), 1);
        let PersistPolicy::Durable {
            local_slot,
            persist_kind,
            ..
        } = program.ir.persist_policy(NodeId(3004))
        else {
            panic!("list_retain_reactive persistence should be durable");
        };
        assert_eq!(local_slot, 0);
        assert_eq!(persist_kind, PersistKind::Hold);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_list_map_external_dep_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_map_external_dep/list_map_external_dep.bn"
        );
        let program =
            try_lower_list_map_external_dep(source).expect("list_map_external_dep should lower");
        assert_eq!(program.toggle_port, SourcePortId(40));
        assert_eq!(program.mode_sink, SinkPortId(40));
        assert_eq!(program.info_sink, SinkPortId(41));
        assert_eq!(program.items_list_sink, SinkPortId(46));
        assert!(program.ir.functions.is_empty());
        assert!(matches!(
            program.ir.nodes.iter().find(|node| node.id == NodeId(4017)),
            Some(IrNode {
                kind: IrNodeKind::When { .. },
                ..
            })
        ));
        assert_eq!(
            program.item_sinks,
            [
                SinkPortId(42),
                SinkPortId(43),
                SinkPortId(44),
                SinkPortId(45)
            ]
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_list_map_block_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_map_block/list_map_block.bn"
        );
        let program = try_lower_list_map_block(source).expect("list_map_block should lower");
        assert_eq!(program.mode_sink, SinkPortId(50));
        assert_eq!(
            program.direct_item_sinks,
            [
                SinkPortId(51),
                SinkPortId(52),
                SinkPortId(53),
                SinkPortId(54),
                SinkPortId(55)
            ]
        );
        assert_eq!(
            program.block_item_sinks,
            [
                SinkPortId(56),
                SinkPortId(57),
                SinkPortId(58),
                SinkPortId(59),
                SinkPortId(60)
            ]
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_list_retain_count_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
        );
        let program = try_lower_list_retain_count(source).expect("list_retain_count should lower");
        assert_eq!(program.input_sink, SinkPortId(70));
        assert_eq!(program.all_count_sink, SinkPortId(71));
        assert_eq!(program.retain_count_sink, SinkPortId(72));
        assert_eq!(program.items_list_sink, SinkPortId(77));
        assert_eq!(program.input_change_port, SourcePortId(70));
        assert_eq!(program.input_key_down_port, SourcePortId(71));
        assert_eq!(program.ir.nodes.len(), 30);
        assert_eq!(
            program.item_sinks,
            [
                SinkPortId(73),
                SinkPortId(74),
                SinkPortId(75),
                SinkPortId(76)
            ]
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_list_object_state_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
        );
        let program = try_lower_list_object_state(source).expect("list_object_state should lower");
        assert_eq!(
            program.press_ports,
            [SourcePortId(90), SourcePortId(91), SourcePortId(92)]
        );
        assert_eq!(
            program.count_sinks,
            [SinkPortId(90), SinkPortId(91), SinkPortId(92)]
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_list_retain_remove_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_remove/list_retain_remove.bn"
        );
        let program =
            try_lower_list_retain_remove(source).expect("list_retain_remove should lower");
        assert_eq!(program.ir.nodes.len(), 26);
        assert_eq!(program.title_sink, SinkPortId(80));
        assert_eq!(program.input_sink, SinkPortId(81));
        assert_eq!(program.count_sink, SinkPortId(82));
        assert_eq!(program.items_list_sink, SinkPortId(89));
        assert_eq!(program.input_change_port, SourcePortId(80));
        assert_eq!(program.input_key_down_port, SourcePortId(81));
        assert_eq!(
            program.item_sinks,
            [
                SinkPortId(83),
                SinkPortId(84),
                SinkPortId(85),
                SinkPortId(86),
                SinkPortId(87),
                SinkPortId(88)
            ]
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_shopping_list_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
        );
        let program = try_lower_shopping_list(source).expect("shopping_list should lower");
        assert_eq!(program.title_sink, SinkPortId(1006));
        assert_eq!(program.input_sink, SinkPortId(1000));
        assert_eq!(program.count_sink, SinkPortId(1001));
        assert_eq!(program.items_list_sink, SinkPortId(1007));
        assert_eq!(program.input_change_port, SourcePortId(1000));
        assert_eq!(program.input_key_down_port, SourcePortId(1001));
        assert_eq!(program.clear_press_port, SourcePortId(1002));
        assert_eq!(program.ir.nodes.len(), 28);
        assert_eq!(
            program.item_sinks,
            [
                SinkPortId(1002),
                SinkPortId(1003),
                SinkPortId(1004),
                SinkPortId(1005)
            ]
        );
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_flight_booker_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
        );
        let program = try_lower_flight_booker(source).expect("flight_booker should lower");
        assert_eq!(program.title_sink, SinkPortId(1900));
        assert_eq!(program.selected_flight_type_sink, SinkPortId(1901));
        assert_eq!(program.departure_input_sink, SinkPortId(1902));
        assert_eq!(program.return_input_sink, SinkPortId(1903));
        assert_eq!(program.return_input_disabled_sink, SinkPortId(1904));
        assert_eq!(program.book_button_disabled_sink, SinkPortId(1905));
        assert_eq!(program.booked_sink, SinkPortId(1906));
        assert_eq!(program.flight_type_change_port, SourcePortId(1900));
        assert_eq!(program.departure_change_port, SourcePortId(1901));
        assert_eq!(program.return_change_port, SourcePortId(1902));
        assert_eq!(program.book_press_port, SourcePortId(1903));
        assert_eq!(program.ir.nodes.len(), 36);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_timer_example() {
        let source = include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let program = try_lower_timer(source).expect("timer should lower");
        assert_eq!(program.title_sink, SinkPortId(1950));
        assert_eq!(program.duration_change_port, SourcePortId(1950));
        assert_eq!(program.reset_press_port, SourcePortId(1951));
        assert_eq!(program.tick_port, SourcePortId(1952));
        assert_eq!(program.duration_slider_sink, SinkPortId(1955));
        assert_eq!(program.ir.nodes.len(), 39);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_circle_drawer_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
        );
        let program = try_lower_circle_drawer(source).expect("circle_drawer should lower");
        assert_eq!(program.title_sink, SinkPortId(1970));
        assert_eq!(program.count_sink, SinkPortId(1971));
        assert_eq!(program.circles_sink, SinkPortId(1972));
        assert_eq!(program.canvas_click_port, SourcePortId(1970));
        assert_eq!(program.undo_press_port, SourcePortId(1971));
        assert_eq!(program.ir.nodes.len(), 18);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_temperature_converter_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let program =
            try_lower_temperature_converter(source).expect("temperature_converter should lower");
        assert_eq!(program.title_sink, SinkPortId(1800));
        assert_eq!(program.celsius_input_sink, SinkPortId(1801));
        assert_eq!(program.fahrenheit_input_sink, SinkPortId(1802));
        assert_eq!(program.celsius_change_port, SourcePortId(1800));
        assert_eq!(program.fahrenheit_change_port, SourcePortId(1802));
        assert_eq!(program.fahrenheit_label_sink, SinkPortId(1805));
        assert_eq!(program.ir.nodes.len(), 40);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_filter_checkbox_bug_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/filter_checkbox_bug/filter_checkbox_bug.bn"
        );
        let program =
            try_lower_filter_checkbox_bug(source).expect("filter_checkbox_bug should lower");
        assert_eq!(program.filter_all_port, SourcePortId(1200));
        assert_eq!(program.filter_active_port, SourcePortId(1201));
        assert_eq!(
            program.checkbox_ports,
            [SourcePortId(1202), SourcePortId(1203)]
        );
        assert_eq!(program.checkbox_sinks, [SinkPortId(1201), SinkPortId(1202)]);
        assert_eq!(
            program.item_label_sinks,
            [SinkPortId(1203), SinkPortId(1204)]
        );
        assert_eq!(program.footer_sink, SinkPortId(1205));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_checkbox_test_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/checkbox_test/checkbox_test.bn"
        );
        let program = try_lower_checkbox_test(source).expect("checkbox_test should lower");
        assert_eq!(
            program.checkbox_ports,
            [SourcePortId(1300), SourcePortId(1301)]
        );
        assert_eq!(program.checkbox_sinks, [SinkPortId(1300), SinkPortId(1301)]);
        assert_eq!(program.label_sinks, [SinkPortId(1304), SinkPortId(1305)]);
        assert_eq!(program.status_sinks, [SinkPortId(1302), SinkPortId(1303)]);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_chained_list_remove_bug_example() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/chained_list_remove_bug/chained_list_remove_bug.bn"
        );
        let program = try_lower_chained_list_remove_bug(source)
            .expect("chained_list_remove_bug should lower");
        assert_eq!(program.add_press_port, SourcePortId(1400));
        assert_eq!(program.clear_completed_port, SourcePortId(1401));
        assert_eq!(program.checkbox_ports[0], SourcePortId(1402));
        assert_eq!(program.remove_ports[3], SourcePortId(1413));
        assert_eq!(program.checkbox_sinks[0], SinkPortId(1400));
        assert_eq!(program.row_label_sinks[3], SinkPortId(1407));
        assert_eq!(program.counts_sink, SinkPortId(1408));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_crud_example() {
        let source = include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let program = try_lower_crud(source).expect("crud should lower");
        assert_eq!(program.title_sink, SinkPortId(1600));
        assert_eq!(program.filter_input_sink, SinkPortId(1601));
        assert_eq!(program.name_input_sink, SinkPortId(1602));
        assert_eq!(program.surname_input_sink, SinkPortId(1603));
        assert_eq!(program.create_press_port, SourcePortId(1606));
        assert_eq!(program.delete_press_port, SourcePortId(1608));
        assert_eq!(program.row_press_ports[0], SourcePortId(1609));
        assert_eq!(program.row_label_sinks[3], SinkPortId(1607));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_todo_mvc_example() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc should lower");
        assert_eq!(
            program.selected_filter_sink,
            TodoProgram::SELECTED_FILTER_SINK
        );
        assert_eq!(program.ir.nodes.len(), 145);
        assert_eq!(program.ir.persistence.len(), 1);
        let policy = program.ir.persist_policy(TodoProgram::TODOS_LIST_HOLD_NODE);
        let PersistPolicy::Durable {
            root_key,
            local_slot,
            persist_kind,
        } = policy
        else {
            panic!("todo_mvc should carry durable list-store metadata");
        };
        assert_ne!(root_key.as_u128(), 0);
        assert_eq!(local_slot, 0);
        assert_eq!(persist_kind, PersistKind::ListStore);
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_todo_mvc_physical_example() {
        let source =
            include_str!("../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn");
        let program = try_lower_todo_mvc_physical(source).expect("todo_mvc_physical should lower");
        let _ = program;
    }

    #[test]
    fn shared_lowering_persistence_helper_collects_hold_and_list_store_specs() {
        let expressions = parse_static_expressions(
            r#"
store: [
    todos: LIST { 1 }
    show_even: True
]
counter: True
"#,
        )
        .expect("persistence test source should parse");
        let bindings = top_level_bindings(&expressions);

        let mut persistence = collect_path_lowering_persistence_from_seed(
            &bindings,
            LoweringPathPersistenceSeed {
                path: &["counter"],
                local_slot: 0,
                persist_kind: PersistKind::Hold,
            },
            NodeId(1),
        );
        persistence.extend(collect_path_lowering_persistence_from_config(
            &bindings,
            LoweringPathPersistenceConfig {
                path: &["store", "todos"],
                node: NodeId(2),
                local_slot: 1,
                persist_kind: PersistKind::ListStore,
            },
        ));
        persistence.extend(collect_path_lowering_persistence_from_seed(
            &bindings,
            LoweringPathPersistenceSeed {
                path: &["store", "missing"],
                local_slot: 2,
                persist_kind: PersistKind::Hold,
            },
            NodeId(3),
        ));

        assert_eq!(persistence.len(), 2);

        let PersistPolicy::Durable {
            root_key,
            local_slot,
            persist_kind,
        } = persistence[0].policy
        else {
            panic!("hold persistence should be durable");
        };
        assert_ne!(root_key.as_u128(), 0);
        assert_eq!(local_slot, 0);
        assert_eq!(persist_kind, PersistKind::Hold);

        let PersistPolicy::Durable {
            root_key,
            local_slot,
            persist_kind,
        } = persistence[1].policy
        else {
            panic!("list-store persistence should be durable");
        };
        assert_ne!(root_key.as_u128(), 0);
        assert_eq!(local_slot, 1);
        assert_eq!(persist_kind, PersistKind::ListStore);
    }

    #[test]
    fn shared_path_lowering_persistence_helper_collects_multiple_configs() {
        let expressions = parse_static_expressions(
            r#"
store: [
    todos: LIST { 1 }
    show_even: True
]
"#,
        )
        .expect("path persistence test source should parse");
        let bindings = top_level_bindings(&expressions);
        let persistence = collect_path_lowering_persistence_from_configs(
            &bindings,
            &[
                LoweringPathPersistenceConfig {
                    path: &["store", "show_even"],
                    node: NodeId(10),
                    local_slot: 0,
                    persist_kind: PersistKind::Hold,
                },
                LoweringPathPersistenceConfig {
                    path: &["store", "todos"],
                    node: NodeId(11),
                    local_slot: 1,
                    persist_kind: PersistKind::ListStore,
                },
                LoweringPathPersistenceConfig {
                    path: &["store", "missing"],
                    node: NodeId(12),
                    local_slot: 2,
                    persist_kind: PersistKind::Hold,
                },
            ],
        );

        assert_eq!(persistence.len(), 2);

        let PersistPolicy::Durable {
            root_key,
            local_slot,
            persist_kind,
        } = persistence[0].policy
        else {
            panic!("first config should be durable");
        };
        assert_ne!(root_key.as_u128(), 0);
        assert_eq!(local_slot, 0);
        assert_eq!(persist_kind, PersistKind::Hold);

        let PersistPolicy::Durable {
            root_key,
            local_slot,
            persist_kind,
        } = persistence[1].policy
        else {
            panic!("second config should be durable");
        };
        assert_ne!(root_key.as_u128(), 0);
        assert_eq!(local_slot, 1);
        assert_eq!(persist_kind, PersistKind::ListStore);
    }

    #[test]
    fn lowers_static_document_examples_into_host_view_ir() {
        let minimal = include_str!("../../../playground/frontend/src/examples/minimal/minimal.bn");
        let hello_world =
            include_str!("../../../playground/frontend/src/examples/hello_world/hello_world.bn");

        let minimal_program = try_lower_static_document(minimal).expect("minimal should lower");
        let hello_program =
            try_lower_static_document(hello_world).expect("hello_world should lower");

        assert!(minimal_program.host_view.root.is_some());
        assert_eq!(
            minimal_program.sink_values.get(&SinkPortId(200)),
            Some(&KernelValue::from(123.0))
        );
        assert_eq!(
            hello_program.sink_values.get(&SinkPortId(200)),
            Some(&KernelValue::from("Hello world!"))
        );
    }

    #[test]
    fn lowerer_source_does_not_reintroduce_subset_enums_or_config_router_helpers() {
        let source = include_str!("lower_legacy.rs");
        let surface_token = "Surface";
        let bindings_token = "Bindings";
        let subset_token = "Subset";
        let program_mode_token = "ProgramMode";
        let config_suffix = "_config(";
        let dynamic_piece_a = "SurfaceProgramSource";
        let dynamic_piece_b = "Dynamic(";
        let bindings_config_const = "_BINDINGS_CONFIG";
        let cases_const = "_CASES";
        let wrapper_helper_prefix = "fn wrap_";
        let host_view_builder_type = "type HostViewProgramBuilder =";
        let sink_values_builder_type = "type HostViewSinkValuesProgramBuilder =";
        let ir_host_view_builder_type = "type IrHostViewProgramBuilder =";
        let indexed_text_grid_builder_type = "type IndexedTextGridProgramBuilder =";
        let press_driven_accumulator_builder_type = "type PressDrivenAccumulatorProgramBuilder =";
        let form_runtime_builder_type = "type FormRuntimeLoweredProgramBuilder =";
        let timed_math_builder_type = "type TimedMathLoweredProgramBuilder =";
        let program_mode_field = "program_mode:";
        let example_inventory_const_prefixes = [
            "const SHOPPING_LIST_",
            "const CRUD_",
            "const TEMPERATURE_CONVERTER_",
            "const FLIGHT_BOOKER_",
            "const TIMER_",
            "const THEN_TIMED_FLOW_",
            "const WHEN_TIMED_FLOW_",
            "const WHILE_TIMED_FLOW_",
        ];
        let example_lowered_builder_prefixes = [
            "fn build_todo_mvc_lowered_program(",
            "fn build_circle_drawer_lowered_program(",
            "fn build_complex_counter_lowered_program(",
            "fn build_filter_checkbox_bug_lowered_program(",
            "fn build_chained_list_remove_bug_lowered_program(",
            "fn build_shopping_list_lowered_program(",
            "fn build_crud_lowered_program(",
            "fn build_fibonacci_lowered_program(",
            "fn build_temperature_converter_lowered_program(",
            "fn build_flight_booker_lowered_program(",
            "fn build_timer_lowered_program(",
            "fn build_pages_lowered_program(",
            "fn build_latest_lowered_program(",
            "fn build_text_interpolation_update_lowered_program(",
            "fn build_button_hover_to_click_test_lowered_program(",
            "fn build_button_hover_test_lowered_program(",
            "fn build_while_function_call_lowered_program(",
            "fn build_switch_hold_test_lowered_program(",
            "fn build_list_map_block_lowered_program(",
            "fn build_list_object_state_lowered_program(",
            "fn build_list_map_external_dep_lowered_program(",
            "fn build_list_retain_reactive_lowered_program(",
            "fn build_list_retain_count_lowered_program(",
            "fn build_list_retain_remove_lowered_program(",
            "fn build_layers_lowered_program(",
            "fn build_counter_lowered_program(",
            "fn build_checkbox_test_lowered_program(",
            "fn build_cells_lowered_program(",
            "fn build_then_lowered_program(",
            "fn build_when_lowered_program(",
            "fn build_while_lowered_program(",
        ];
        let business_ir_builder_prefixes = [
            "fn build_canvas_undo_list_ir(",
            "fn build_mirrored_accumulator_buttons_ir(",
            "fn build_dual_temperature_input_ir(",
            "fn build_selectable_trip_dates_ir(",
            "fn build_timed_progress_reset_ir(",
        ];
        let example_subset_literals = [
            "subset: \"pages\"",
            "subset: \"latest\"",
            "subset: \"text_interpolation_update\"",
            "subset: \"button_hover_to_click_test\"",
            "subset: \"button_hover_test\"",
            "subset: \"while_function_call\"",
            "subset: \"switch_hold_test\"",
            "subset: \"filter_checkbox_bug\"",
            "subset: \"checkbox_test\"",
            "subset: \"chained_list_remove_bug\"",
            "subset: \"list_map_block\"",
            "subset: \"list_object_state\"",
            "subset: \"layers\"",
            "subset: \"fibonacci\"",
            "subset: \"static\"",
            "subset: \"circle_drawer\"",
            "subset: \"complex_counter\"",
            "subset: \"counter\"",
            "subset: \"todo_mvc\"",
            "subset: \"list_map_external_dep\"",
            "subset: \"list_retain_reactive\"",
            "subset: \"list_retain_count\"",
            "subset: \"list_retain_remove\"",
            "subset: \"shopping_list\"",
            "subset: \"crud\"",
            "subset: \"cells\"",
            "subset: \"todo_mvc_physical\"",
            "subset: \"temperature_converter\"",
            "subset: \"flight_booker\"",
            "subset: \"timer\"",
            "subset: \"interval\"",
            "subset: \"interval_hold\"",
            "subset: \"then\"",
            "subset: \"when\"",
            "subset: \"while\"",
        ];
        let direct_persistence_call_count = source
            .lines()
            .filter(|&line| {
                let trimmed = line.trim();
                trimmed.starts_with("persist_entry_for_path(")
            })
            .count();

        let mut banned_lines = Vec::new();

        for line in source.lines() {
            let trimmed = line.trim();
            let has_subset_enum = trimmed.starts_with("enum ")
                && trimmed.contains(subset_token)
                && (trimmed.contains(surface_token) || trimmed.contains(bindings_token));
            let has_program_mode_enum =
                trimmed.starts_with("enum ") && trimmed.contains(program_mode_token);
            let has_router_helper = trimmed.starts_with("fn ")
                && trimmed.contains(config_suffix)
                && !trimmed.contains("_from_config(");
            let has_dynamic_surface_case =
                trimmed.contains(dynamic_piece_a) && trimmed.contains(dynamic_piece_b);
            let has_bindings_config_const =
                trimmed.starts_with("const ") && trimmed.contains(bindings_config_const);
            let has_cases_const = trimmed.starts_with("const ") && trimmed.contains(cases_const);
            let has_wrapper_helper = trimmed.starts_with(wrapper_helper_prefix)
                && !trimmed.starts_with("fn wrap_lowered_program(");
            let has_host_view_builder_type = trimmed.starts_with(host_view_builder_type);
            let has_sink_values_builder_type = trimmed.starts_with(sink_values_builder_type);
            let has_ir_host_view_builder_type = trimmed.starts_with(ir_host_view_builder_type);
            let has_indexed_text_grid_builder_type =
                trimmed.starts_with(indexed_text_grid_builder_type);
            let has_press_driven_accumulator_builder_type =
                trimmed.starts_with(press_driven_accumulator_builder_type);
            let has_form_runtime_builder_type = trimmed.starts_with(form_runtime_builder_type);
            let has_timed_math_builder_type = trimmed.starts_with(timed_math_builder_type);
            let has_program_mode_field = trimmed.contains(program_mode_field)
                && !trimmed.starts_with("let program_mode_field = ");
            let has_example_subset_literal = example_subset_literals
                .iter()
                .any(|literal| trimmed.contains(literal));
            let has_example_inventory_const = example_inventory_const_prefixes
                .iter()
                .any(|prefix| trimmed.starts_with(prefix));
            let has_example_lowered_builder = example_lowered_builder_prefixes
                .iter()
                .any(|prefix| trimmed.starts_with(prefix));
            let has_business_ir_builder = business_ir_builder_prefixes
                .iter()
                .any(|prefix| trimmed.starts_with(prefix));

            if has_subset_enum
                || has_program_mode_enum
                || has_router_helper
                || has_dynamic_surface_case
                || has_bindings_config_const
                || has_cases_const
                || has_wrapper_helper
                || has_host_view_builder_type
                || has_sink_values_builder_type
                || has_ir_host_view_builder_type
                || has_indexed_text_grid_builder_type
                || has_press_driven_accumulator_builder_type
                || has_form_runtime_builder_type
                || has_timed_math_builder_type
                || has_program_mode_field
                || has_example_subset_literal
                || has_example_inventory_const
                || has_example_lowered_builder
                || has_business_ir_builder
            {
                banned_lines.push(trimmed.to_string());
            }
        }

        assert!(
            banned_lines.is_empty(),
            "lower.rs reintroduced banned lowering-routing seams: {banned_lines:#?}"
        );
        assert_eq!(
            direct_persistence_call_count, 1,
            "lower.rs should route persistence collection through the shared collect_path_lowering_persistence(...) helper"
        );
    }
}
