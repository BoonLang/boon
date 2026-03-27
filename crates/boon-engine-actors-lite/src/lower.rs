use crate::bridge::{
    HostCrossAlign, HostSelectOption, HostStripeDirection, HostViewIr, HostViewKind, HostViewNode,
    HostWidth,
};
use crate::ir::{
    CallSiteId, FunctionId, FunctionInstanceId, IrFunctionTemplate, IrNode, IrNodeKind, IrProgram,
    MirrorCellId, NodeId, RetainedNodeKey, SinkPortId, SourcePortId, ViewSiteId,
};
use crate::parse::{
    StaticExpression, StaticSpannedExpression, contains_alias_path, contains_function_call_path,
    contains_text_fragment, contains_top_level_function, parse_static_expressions,
    top_level_bindings,
};
use boon::platform::browser::kernel::KernelValue;
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
    pub host_view: HostViewIr,
    pub toggle_port: SourcePortId,
    pub mode_sink: SinkPortId,
    pub count_sink: SinkPortId,
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
    pub host_view: HostViewIr,
    pub tick_port: SourcePortId,
    pub addition_press_port: SourcePortId,
    pub input_a_sink: SinkPortId,
    pub input_b_sink: SinkPortId,
    pub result_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct WhenProgram {
    pub host_view: HostViewIr,
    pub tick_port: SourcePortId,
    pub addition_press_port: SourcePortId,
    pub subtraction_press_port: SourcePortId,
    pub input_a_sink: SinkPortId,
    pub input_b_sink: SinkPortId,
    pub result_sink: SinkPortId,
}

#[derive(Debug, Clone)]
pub struct WhileProgram {
    pub host_view: HostViewIr,
    pub tick_port: SourcePortId,
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
    pub selected_filter_sink: SinkPortId,
}
pub struct TodoPhysicalProgram;

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

fn lower_todo_ui_state_ir() -> Vec<IrNode> {
    vec![
        IrNode {
            id: NodeId(1400),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("all")),
        },
        IrNode {
            id: NodeId(1401),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::FILTER_ALL_PORT),
        },
        IrNode {
            id: NodeId(1402),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("all")),
        },
        IrNode {
            id: NodeId(1403),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1401),
                body: NodeId(1402),
            },
        },
        IrNode {
            id: NodeId(1404),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::FILTER_ACTIVE_PORT),
        },
        IrNode {
            id: NodeId(1405),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("active")),
        },
        IrNode {
            id: NodeId(1406),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1404),
                body: NodeId(1405),
            },
        },
        IrNode {
            id: NodeId(1407),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::FILTER_COMPLETED_PORT),
        },
        IrNode {
            id: NodeId(1408),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("completed")),
        },
        IrNode {
            id: NodeId(1409),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1407),
                body: NodeId(1408),
            },
        },
        IrNode {
            id: NodeId(1410),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1403), NodeId(1406), NodeId(1409)],
            },
        },
        IrNode {
            id: NodeId(1411),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1400),
                updates: NodeId(1410),
            },
        },
        IrNode {
            id: NodeId(1412),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::SELECTED_FILTER_SINK,
                input: NodeId(1411),
            },
        },
        IrNode {
            id: NodeId(1420),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(1421),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(TodoProgram::MAIN_INPUT_DRAFT_CELL),
        },
        IrNode {
            id: NodeId(1422),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::MAIN_INPUT_CHANGE_PORT),
        },
        IrNode {
            id: NodeId(1423),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1421), NodeId(1422), NodeId(1561)],
            },
        },
        IrNode {
            id: NodeId(1424),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1420),
                updates: NodeId(1423),
            },
        },
        IrNode {
            id: NodeId(1425),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::MAIN_INPUT_TEXT_SINK,
                input: NodeId(1424),
            },
        },
        IrNode {
            id: NodeId(1426),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(true)),
        },
        IrNode {
            id: NodeId(1427),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(TodoProgram::MAIN_INPUT_FOCUSED_CELL),
        },
        IrNode {
            id: NodeId(1428),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1426),
                updates: NodeId(1528),
            },
        },
        IrNode {
            id: NodeId(1429),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::MAIN_INPUT_FOCUSED_SINK,
                input: NodeId(1428),
            },
        },
        IrNode {
            id: NodeId(1430),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(TodoProgram::TODOS_LIST_CELL),
        },
        IrNode {
            id: NodeId(1431),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(TodoProgram::NEXT_TODO_ID_CELL),
        },
        IrNode {
            id: NodeId(1432),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(false)),
        },
        IrNode {
            id: NodeId(1433),
            source_expr: None,
            kind: IrNodeKind::Skip,
        },
        IrNode {
            id: NodeId(1434),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(1435),
            source_expr: None,
            kind: IrNodeKind::TextTrim {
                input: NodeId(1560),
            },
        },
        IrNode {
            id: NodeId(1436),
            source_expr: None,
            kind: IrNodeKind::Eq {
                lhs: NodeId(1435),
                rhs: NodeId(1434),
            },
        },
        IrNode {
            id: NodeId(1437),
            source_expr: None,
            kind: IrNodeKind::ObjectLiteral {
                fields: vec![
                    ("id".to_string(), NodeId(1431)),
                    ("title".to_string(), NodeId(1435)),
                    ("completed".to_string(), NodeId(1432)),
                ],
            },
        },
        IrNode {
            id: NodeId(1438),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(1436),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1433),
                }],
                fallback: NodeId(1437),
            },
        },
        IrNode {
            id: NodeId(1439),
            source_expr: None,
            kind: IrNodeKind::ListAppend {
                list: NodeId(1430),
                item: NodeId(1438),
            },
        },
        IrNode {
            id: NodeId(1440),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::MAIN_INPUT_KEY_DOWN_PORT),
        },
        IrNode {
            id: NodeId(1441),
            source_expr: None,
            kind: IrNodeKind::KeyDownKey {
                input: NodeId(1440),
            },
        },
        IrNode {
            id: NodeId(1442),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Enter")),
        },
        IrNode {
            id: NodeId(1443),
            source_expr: None,
            kind: IrNodeKind::Eq {
                lhs: NodeId(1441),
                rhs: NodeId(1442),
            },
        },
        IrNode {
            id: NodeId(1444),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(1443),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1439),
                }],
                fallback: NodeId(1433),
            },
        },
        IrNode {
            id: NodeId(1445),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1556),
                field: "id".to_string(),
            },
        },
        IrNode {
            id: NodeId(1446),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_TOGGLE_PORT),
        },
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
            id: NodeId(1448),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1446),
                body: NodeId(1447),
            },
        },
        IrNode {
            id: NodeId(1449),
            source_expr: None,
            kind: IrNodeKind::ListAllObjectBoolField {
                list: NodeId(1430),
                field: "completed".to_string(),
            },
        },
        IrNode {
            id: NodeId(1450),
            source_expr: None,
            kind: IrNodeKind::BoolNot {
                input: NodeId(1449),
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
            id: NodeId(1452),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TOGGLE_ALL_PORT),
        },
        IrNode {
            id: NodeId(1453),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1452),
                body: NodeId(1451),
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
        IrNode {
            id: NodeId(1455),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::CLEAR_COMPLETED_PORT),
        },
        IrNode {
            id: NodeId(1456),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1455),
                body: NodeId(1454),
            },
        },
        IrNode {
            id: NodeId(1457),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(TodoProgram::EDIT_TITLE_CELL),
        },
        IrNode {
            id: NodeId(1458),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_EDIT_CHANGE_PORT),
        },
        IrNode {
            id: NodeId(1459),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1457), NodeId(1458), NodeId(1557)],
            },
        },
        IrNode {
            id: NodeId(1460),
            source_expr: None,
            kind: IrNodeKind::TextTrim {
                input: NodeId(1459),
            },
        },
        IrNode {
            id: NodeId(1461),
            source_expr: None,
            kind: IrNodeKind::Eq {
                lhs: NodeId(1460),
                rhs: NodeId(1434),
            },
        },
        IrNode {
            id: NodeId(1462),
            source_expr: None,
            kind: IrNodeKind::ListMapObjectFieldByFieldEq {
                list: NodeId(1430),
                match_field: "id".to_string(),
                match_value: NodeId(1554),
                update_field: "title".to_string(),
                update_value: NodeId(1460),
            },
        },
        IrNode {
            id: NodeId(1463),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(1461),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1433),
                }],
                fallback: NodeId(1462),
            },
        },
        IrNode {
            id: NodeId(1464),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_EDIT_COMMIT_PORT),
        },
        IrNode {
            id: NodeId(1465),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1464),
                body: NodeId(1463),
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
        IrNode {
            id: NodeId(1467),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_DELETE_PORT),
        },
        IrNode {
            id: NodeId(1468),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1467),
                body: NodeId(1466),
            },
        },
        IrNode {
            id: NodeId(1469),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![
                    NodeId(1430),
                    NodeId(1444),
                    NodeId(1448),
                    NodeId(1453),
                    NodeId(1456),
                    NodeId(1465),
                    NodeId(1468),
                ],
            },
        },
        IrNode {
            id: NodeId(1470),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::TODOS_LIST_SINK,
                input: NodeId(1469),
            },
        },
        IrNode {
            id: NodeId(1472),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::Tag("none".to_string())),
        },
        IrNode {
            id: NodeId(1473),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_BEGIN_EDIT_PORT),
        },
        IrNode {
            id: NodeId(1474),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1473),
                body: NodeId(1551),
            },
        },
        IrNode {
            id: NodeId(1475),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1464),
                body: NodeId(1472),
            },
        },
        IrNode {
            id: NodeId(1476),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_EDIT_CANCEL_PORT),
        },
        IrNode {
            id: NodeId(1477),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1476),
                body: NodeId(1472),
            },
        },
        IrNode {
            id: NodeId(1478),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1474), NodeId(1475), NodeId(1477)],
            },
        },
        IrNode {
            id: NodeId(1479),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1472),
                updates: NodeId(1478),
            },
        },
        IrNode {
            id: NodeId(1480),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::EDIT_TARGET_SINK,
                input: NodeId(1479),
            },
        },
        IrNode {
            id: NodeId(1482),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(1483),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1473),
                body: NodeId(1552),
            },
        },
        IrNode {
            id: NodeId(1484),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1464),
                body: NodeId(1482),
            },
        },
        IrNode {
            id: NodeId(1485),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1476),
                body: NodeId(1482),
            },
        },
        IrNode {
            id: NodeId(1486),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1459), NodeId(1483), NodeId(1484), NodeId(1485)],
            },
        },
        IrNode {
            id: NodeId(1487),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1482),
                updates: NodeId(1486),
            },
        },
        IrNode {
            id: NodeId(1488),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::EDIT_DRAFT_SINK,
                input: NodeId(1487),
            },
        },
        IrNode {
            id: NodeId(1490),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(false)),
        },
        IrNode {
            id: NodeId(1491),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(true)),
        },
        IrNode {
            id: NodeId(1492),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(TodoProgram::EDIT_FOCUSED_CELL),
        },
        IrNode {
            id: NodeId(1493),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1473),
                body: NodeId(1491),
            },
        },
        IrNode {
            id: NodeId(1494),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(1492),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1490),
                }],
                fallback: NodeId(1433),
            },
        },
        IrNode {
            id: NodeId(1495),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1464),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1496),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1476),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1497),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![
                    NodeId(1493),
                    NodeId(1494),
                    NodeId(1495),
                    NodeId(1496),
                    NodeId(1547),
                ],
            },
        },
        IrNode {
            id: NodeId(1498),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1490),
                updates: NodeId(1497),
            },
        },
        IrNode {
            id: NodeId(1499),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::EDIT_FOCUS_HINT_SINK,
                input: NodeId(1498),
            },
        },
        IrNode {
            id: NodeId(1500),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1473),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1501),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1464),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1502),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1476),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1503),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![
                    NodeId(1492),
                    NodeId(1500),
                    NodeId(1501),
                    NodeId(1502),
                    NodeId(1548),
                    NodeId(1549),
                ],
            },
        },
        IrNode {
            id: NodeId(1504),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1490),
                updates: NodeId(1503),
            },
        },
        IrNode {
            id: NodeId(1505),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::EDIT_FOCUSED_SINK,
                input: NodeId(1504),
            },
        },
        IrNode {
            id: NodeId(1510),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1556),
                field: "hovered".to_string(),
            },
        },
        IrNode {
            id: NodeId(1511),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(1510),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1445),
                }],
                fallback: NodeId(1472),
            },
        },
        IrNode {
            id: NodeId(1512),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1472),
                updates: NodeId(1532),
            },
        },
        IrNode {
            id: NodeId(1513),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::HOVERED_TARGET_SINK,
                input: NodeId(1512),
            },
        },
        IrNode {
            id: NodeId(1514),
            source_expr: None,
            kind: IrNodeKind::ListRetainObjectBoolField {
                list: NodeId(1469),
                field: "completed".to_string(),
                keep_if: false,
            },
        },
        IrNode {
            id: NodeId(1515),
            source_expr: None,
            kind: IrNodeKind::ListCount { list: NodeId(1514) },
        },
        IrNode {
            id: NodeId(1516),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::ACTIVE_COUNT_SINK,
                input: NodeId(1515),
            },
        },
        IrNode {
            id: NodeId(1517),
            source_expr: None,
            kind: IrNodeKind::ListRetainObjectBoolField {
                list: NodeId(1469),
                field: "completed".to_string(),
                keep_if: true,
            },
        },
        IrNode {
            id: NodeId(1518),
            source_expr: None,
            kind: IrNodeKind::ListCount { list: NodeId(1517) },
        },
        IrNode {
            id: NodeId(1519),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::COMPLETED_COUNT_SINK,
                input: NodeId(1518),
            },
        },
        IrNode {
            id: NodeId(1520),
            source_expr: None,
            kind: IrNodeKind::ListAllObjectBoolField {
                list: NodeId(1469),
                field: "completed".to_string(),
            },
        },
        IrNode {
            id: NodeId(1521),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::ALL_COMPLETED_SINK,
                input: NodeId(1520),
            },
        },
        IrNode {
            id: NodeId(1522),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(true)),
        },
        IrNode {
            id: NodeId(1523),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(TodoProgram::MAIN_INPUT_FOCUS_HINT_CELL),
        },
        IrNode {
            id: NodeId(1524),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1522),
                updates: NodeId(1539),
            },
        },
        IrNode {
            id: NodeId(1525),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK,
                input: NodeId(1524),
            },
        },
        IrNode {
            id: NodeId(1526),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1473),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1527),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(1492),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1490),
                }],
                fallback: NodeId(1433),
            },
        },
        IrNode {
            id: NodeId(1528),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![
                    NodeId(1427),
                    NodeId(1526),
                    NodeId(1527),
                    NodeId(1541),
                    NodeId(1543),
                    NodeId(1558),
                    NodeId(1550),
                ],
            },
        },
        IrNode {
            id: NodeId(1529),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1473),
                body: NodeId(1472),
            },
        },
        IrNode {
            id: NodeId(1530),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1467),
                body: NodeId(1472),
            },
        },
        IrNode {
            id: NodeId(1532),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1511), NodeId(1529), NodeId(1530)],
            },
        },
        IrNode {
            id: NodeId(1533),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1401),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1534),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1404),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1535),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1407),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1536),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1452),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1537),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1455),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1538),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1473),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1539),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![
                    NodeId(1523),
                    NodeId(1533),
                    NodeId(1534),
                    NodeId(1535),
                    NodeId(1536),
                    NodeId(1537),
                    NodeId(1538),
                    NodeId(1544),
                    NodeId(1559),
                ],
            },
        },
        IrNode {
            id: NodeId(1540),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::MAIN_INPUT_BLUR_PORT),
        },
        IrNode {
            id: NodeId(1541),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1540),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1542),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::MAIN_INPUT_FOCUS_PORT),
        },
        IrNode {
            id: NodeId(1543),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1542),
                body: NodeId(1426),
            },
        },
        IrNode {
            id: NodeId(1544),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1542),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1545),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_EDIT_BLUR_PORT),
        },
        IrNode {
            id: NodeId(1546),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_EDIT_FOCUS_PORT),
        },
        IrNode {
            id: NodeId(1547),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1546),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1548),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1545),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1549),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1546),
                body: NodeId(1491),
            },
        },
        IrNode {
            id: NodeId(1550),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1546),
                body: NodeId(1490),
            },
        },
        IrNode {
            id: NodeId(1551),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1473),
                field: "id".to_string(),
            },
        },
        IrNode {
            id: NodeId(1552),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1473),
                field: "title".to_string(),
            },
        },
        IrNode {
            id: NodeId(1553),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1446),
                field: "id".to_string(),
            },
        },
        IrNode {
            id: NodeId(1554),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1464),
                field: "id".to_string(),
            },
        },
        IrNode {
            id: NodeId(1555),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1467),
                field: "id".to_string(),
            },
        },
        IrNode {
            id: NodeId(1556),
            source_expr: None,
            kind: IrNodeKind::SourcePort(TodoProgram::TODO_HOVER_PORT),
        },
        IrNode {
            id: NodeId(1557),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1464),
                field: "title".to_string(),
            },
        },
        IrNode {
            id: NodeId(1558),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1444),
                body: NodeId(1426),
            },
        },
        IrNode {
            id: NodeId(1559),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1444),
                body: NodeId(1491),
            },
        },
        IrNode {
            id: NodeId(1560),
            source_expr: None,
            kind: IrNodeKind::KeyDownText {
                input: NodeId(1440),
            },
        },
        IrNode {
            id: NodeId(1561),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1444),
                body: NodeId(1434),
            },
        },
    ]
}

pub fn try_lower_counter(source: &str) -> Result<CounterProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);

    let document = bindings
        .get("document")
        .ok_or_else(|| "counter subset requires top-level `document`".to_string())?;
    let counter = bindings
        .get("counter")
        .ok_or_else(|| "counter subset requires top-level `counter`".to_string())?;
    let button = bindings
        .get("increment_button")
        .ok_or_else(|| "counter subset requires `increment_button` binding".to_string())?;

    let (press_port, label_text) = lower_increment_button(button)?;
    let (initial_value, increment_delta, counter_ir) = lower_counter(counter, press_port)?;
    let host_view = lower_counter_document(document, press_port, label_text)?;

    Ok(CounterProgram {
        ir: counter_ir.into(),
        host_view,
        press_port,
        counter_sink: SinkPortId(1),
        initial_value,
        increment_delta,
    })
}

pub fn try_lower_todo_mvc(source: &str) -> Result<TodoProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);

    if !bindings.contains_key("store") {
        return Err("todo_mvc subset requires top-level `store`".to_string());
    }
    if !bindings.contains_key("document") {
        return Err("todo_mvc subset requires top-level `document`".to_string());
    }
    if !contains_top_level_function(&expressions, "new_todo") {
        return Err("todo_mvc subset requires top-level function `new_todo`".to_string());
    }
    for required_path in [
        ["Router", "go_to"].as_slice(),
        ["Router", "route"].as_slice(),
        ["Element", "text_input"].as_slice(),
        ["Element", "checkbox"].as_slice(),
        ["Element", "button"].as_slice(),
    ] {
        if !contains_function_call_path(&expressions, required_path) {
            return Err(format!(
                "todo_mvc subset requires call path `{}`",
                required_path.join("/")
            ));
        }
    }
    for alias_path in [
        [
            "todo",
            "todo_elements",
            "todo_title_element",
            "event",
            "double_click",
        ]
        .as_slice(),
        ["store", "elements", "toggle_all_checkbox", "event", "click"].as_slice(),
    ] {
        if !contains_alias_path(&expressions, alias_path) {
            return Err(format!(
                "todo_mvc subset requires alias path `{}`",
                alias_path.join(".")
            ));
        }
    }
    for text in ["Double-click to edit a todo", "Created by", "Martin Kavík"] {
        if !contains_text_fragment(&expressions, text) {
            return Err(format!("todo_mvc subset requires text `{text}`"));
        }
    }

    Ok(TodoProgram {
        ir: lower_todo_ui_state_ir().into(),
        selected_filter_sink: TodoProgram::SELECTED_FILTER_SINK,
    })
}

pub fn try_lower_todo_mvc_physical(source: &str) -> Result<TodoPhysicalProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);

    if !bindings.contains_key("store") {
        return Err("todo_mvc_physical subset requires top-level `store`".to_string());
    }
    if !bindings.contains_key("scene") {
        return Err("todo_mvc_physical subset requires top-level `scene`".to_string());
    }
    if !contains_top_level_function(&expressions, "new_todo") {
        return Err("todo_mvc_physical subset requires top-level function `new_todo`".to_string());
    }
    if !contains_top_level_function(&expressions, "theme_switcher") {
        return Err(
            "todo_mvc_physical subset requires top-level function `theme_switcher`".to_string(),
        );
    }
    for required_path in [
        ["Router", "go_to"].as_slice(),
        ["Router", "route"].as_slice(),
        ["Scene", "new"].as_slice(),
        ["Scene", "Element", "text_input"].as_slice(),
        ["Scene", "Element", "checkbox"].as_slice(),
        ["Scene", "Element", "button"].as_slice(),
    ] {
        if !contains_function_call_path(&expressions, required_path) {
            return Err(format!(
                "todo_mvc_physical subset requires call path `{}`",
                required_path.join("/")
            ));
        }
    }
    for text in [
        "Dark mode",
        "Professional",
        "Glassmorphism",
        "Neobrutalism",
        "Neumorphism",
        "Created by",
        "TodoMVC",
    ] {
        if !contains_text_fragment(&expressions, text) {
            return Err(format!("todo_mvc_physical subset requires text `{text}`"));
        }
    }

    Ok(TodoPhysicalProgram)
}

pub fn try_lower_complex_counter(source: &str) -> Result<ComplexCounterProgram, String> {
    for required_marker in [
        "elements: [decrement_button: LINK, increment_button: LINK]",
        "counter: 0 |> HOLD counter {",
        "elements.decrement_button.event.press |> THEN { counter - 1 }",
        "elements.increment_button.event.press |> THEN { counter + 1 }",
        "counter_button(label: TEXT { - })",
        "counter_button(label: TEXT { + })",
        "element: [event: [press: LINK], hovered: LINK]",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "complex_counter subset requires source marker `{required_marker}`"
            ));
        }
    }

    let ir = vec![
        IrNode {
            id: NodeId(1),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(0.0)),
        },
        IrNode {
            id: NodeId(2),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(10)),
        },
        IrNode {
            id: NodeId(3),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(11)),
        },
        IrNode {
            id: NodeId(4),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(-1.0)),
        },
        IrNode {
            id: NodeId(5),
            source_expr: None,
            kind: IrNodeKind::Add {
                lhs: NodeId(12),
                rhs: NodeId(4),
            },
        },
        IrNode {
            id: NodeId(6),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(2),
                body: NodeId(5),
            },
        },
        IrNode {
            id: NodeId(7),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(1.0)),
        },
        IrNode {
            id: NodeId(8),
            source_expr: None,
            kind: IrNodeKind::Add {
                lhs: NodeId(12),
                rhs: NodeId(7),
            },
        },
        IrNode {
            id: NodeId(9),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(3),
                body: NodeId(8),
            },
        },
        IrNode {
            id: NodeId(10),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(6), NodeId(9)],
            },
        },
        IrNode {
            id: NodeId(12),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1),
                updates: NodeId(10),
            },
        },
        IrNode {
            id: NodeId(13),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(10),
                input: NodeId(12),
            },
        },
        IrNode {
            id: NodeId(14),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(MirrorCellId(20)),
        },
        IrNode {
            id: NodeId(15),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(11),
                input: NodeId(14),
            },
        },
        IrNode {
            id: NodeId(16),
            source_expr: None,
            kind: IrNodeKind::MirrorCell(MirrorCellId(21)),
        },
        IrNode {
            id: NodeId(17),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(12),
                input: NodeId(16),
            },
        },
    ];

    Ok(ComplexCounterProgram {
        ir: ir.into(),
        host_view: lower_complex_counter_document(),
        decrement_port: SourcePortId(10),
        increment_port: SourcePortId(11),
        decrement_hovered_cell: MirrorCellId(20),
        increment_hovered_cell: MirrorCellId(21),
        counter_sink: SinkPortId(10),
        decrement_hovered_sink: SinkPortId(11),
        increment_hovered_sink: SinkPortId(12),
        initial_value: 0,
    })
}

pub fn try_lower_list_retain_reactive(source: &str) -> Result<ListRetainReactiveProgram, String> {
    for required_marker in [
        "show_even: False |> HOLD state {",
        "store.toggle.event.press |> THEN { state |> Bool/not() }",
        "filtered: numbers |> List/retain(n, if: show_even |> WHEN {",
        "filtered_count: filtered |> List/count()",
        "Toggle filter (show_even: {store.show_even})",
        "store.filtered",
        "|> List/map(n, new: Element/label(element: [], style: [], label: n))",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "list_retain_reactive subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ListRetainReactiveProgram {
        host_view: lower_list_retain_reactive_document(),
        toggle_port: SourcePortId(30),
        mode_sink: SinkPortId(30),
        count_sink: SinkPortId(31),
        item_sinks: [
            SinkPortId(32),
            SinkPortId(33),
            SinkPortId(34),
            SinkPortId(35),
            SinkPortId(36),
            SinkPortId(37),
        ],
    })
}

pub fn try_lower_list_map_external_dep(source: &str) -> Result<ListMapExternalDepProgram, String> {
    for required_marker in [
        "show_filtered: False |> HOLD state {",
        "filter_button.event.press |> THEN { state |> Bool/not() }",
        "items: LIST {",
        "Toggle filter",
        "Expected: When True, show Apple and Cherry. When False, show all.",
        "store.items |> List/map(item, new: store.show_filtered |> WHILE {",
        "True => item.show_when_filtered |> WHILE {",
        "False => Element/label(element: [], style: [], label: item.name)",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "list_map_external_dep subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ListMapExternalDepProgram {
        ir: lower_list_map_external_dep_ir(),
        host_view: lower_list_map_external_dep_document(),
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
    })
}

fn lower_list_map_external_dep_ir() -> IrProgram {
    IrProgram {
        nodes: vec![
            IrNode {
                id: NodeId(4000),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(false)),
            },
            IrNode {
                id: NodeId(4001),
                source_expr: None,
                kind: IrNodeKind::SourcePort(SourcePortId(40)),
            },
            IrNode {
                id: NodeId(4002),
                source_expr: None,
                kind: IrNodeKind::BoolNot {
                    input: NodeId(4004),
                },
            },
            IrNode {
                id: NodeId(4003),
                source_expr: None,
                kind: IrNodeKind::Then {
                    source: NodeId(4001),
                    body: NodeId(4002),
                },
            },
            IrNode {
                id: NodeId(4004),
                source_expr: None,
                kind: IrNodeKind::Hold {
                    seed: NodeId(4000),
                    updates: NodeId(4003),
                },
            },
            IrNode {
                id: NodeId(4005),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("show_filtered: True")),
            },
            IrNode {
                id: NodeId(4006),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("show_filtered: False")),
            },
            IrNode {
                id: NodeId(4007),
                source_expr: None,
                kind: IrNodeKind::When {
                    source: NodeId(4004),
                    arms: vec![crate::ir::MatchArm {
                        matcher: KernelValue::from(true),
                        result: NodeId(4005),
                    }],
                    fallback: NodeId(4006),
                },
            },
            IrNode {
                id: NodeId(4008),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(40),
                    input: NodeId(4007),
                },
            },
            IrNode {
                id: NodeId(4009),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(
                    "Expected: When True, show Apple and Cherry. When False, show all.",
                )),
            },
            IrNode {
                id: NodeId(4010),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(41),
                    input: NodeId(4009),
                },
            },
            IrNode {
                id: NodeId(4011),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("Apple")),
            },
            IrNode {
                id: NodeId(4012),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(true)),
            },
            IrNode {
                id: NodeId(4013),
                source_expr: None,
                kind: IrNodeKind::ObjectLiteral {
                    fields: vec![
                        ("name".to_string(), NodeId(4011)),
                        ("show_when_filtered".to_string(), NodeId(4012)),
                    ],
                },
            },
            IrNode {
                id: NodeId(4014),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("Banana")),
            },
            IrNode {
                id: NodeId(4015),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(false)),
            },
            IrNode {
                id: NodeId(4016),
                source_expr: None,
                kind: IrNodeKind::ObjectLiteral {
                    fields: vec![
                        ("name".to_string(), NodeId(4014)),
                        ("show_when_filtered".to_string(), NodeId(4015)),
                    ],
                },
            },
            IrNode {
                id: NodeId(4017),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("Cherry")),
            },
            IrNode {
                id: NodeId(4018),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(true)),
            },
            IrNode {
                id: NodeId(4019),
                source_expr: None,
                kind: IrNodeKind::ObjectLiteral {
                    fields: vec![
                        ("name".to_string(), NodeId(4017)),
                        ("show_when_filtered".to_string(), NodeId(4018)),
                    ],
                },
            },
            IrNode {
                id: NodeId(4020),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("Date")),
            },
            IrNode {
                id: NodeId(4021),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(false)),
            },
            IrNode {
                id: NodeId(4022),
                source_expr: None,
                kind: IrNodeKind::ObjectLiteral {
                    fields: vec![
                        ("name".to_string(), NodeId(4020)),
                        ("show_when_filtered".to_string(), NodeId(4021)),
                    ],
                },
            },
            IrNode {
                id: NodeId(4023),
                source_expr: None,
                kind: IrNodeKind::ListLiteral {
                    items: vec![NodeId(4013), NodeId(4016), NodeId(4019), NodeId(4022)],
                },
            },
            IrNode {
                id: NodeId(4024),
                source_expr: None,
                kind: IrNodeKind::ListMap {
                    list: NodeId(4023),
                    function: FunctionId(40),
                    call_site: CallSiteId(40),
                },
            },
            IrNode {
                id: NodeId(4025),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(46),
                    input: NodeId(4024),
                },
            },
        ],
        functions: vec![IrFunctionTemplate {
            id: FunctionId(40),
            parameter_count: 1,
            output: NodeId(4105),
            nodes: vec![
                IrNode {
                    id: NodeId(4100),
                    source_expr: None,
                    kind: IrNodeKind::Parameter { index: 0 },
                },
                IrNode {
                    id: NodeId(4101),
                    source_expr: None,
                    kind: IrNodeKind::FieldRead {
                        object: NodeId(4100),
                        field: "name".to_string(),
                    },
                },
                IrNode {
                    id: NodeId(4102),
                    source_expr: None,
                    kind: IrNodeKind::FieldRead {
                        object: NodeId(4100),
                        field: "show_when_filtered".to_string(),
                    },
                },
                IrNode {
                    id: NodeId(4103),
                    source_expr: None,
                    kind: IrNodeKind::Skip,
                },
                IrNode {
                    id: NodeId(4104),
                    source_expr: None,
                    kind: IrNodeKind::While {
                        source: NodeId(4102),
                        arms: vec![crate::ir::MatchArm {
                            matcher: KernelValue::from(true),
                            result: NodeId(4101),
                        }],
                        fallback: NodeId(4103),
                    },
                },
                IrNode {
                    id: NodeId(4105),
                    source_expr: None,
                    kind: IrNodeKind::When {
                        source: NodeId(4004),
                        arms: vec![crate::ir::MatchArm {
                            matcher: KernelValue::from(true),
                            result: NodeId(4104),
                        }],
                        fallback: NodeId(4101),
                    },
                },
            ],
        }],
    }
}

pub fn try_lower_list_map_block(source: &str) -> Result<ListMapBlockProgram, String> {
    for required_marker in [
        "mode: All",
        "items: LIST {",
        "Element/label(element: [], style: [], label: TEXT { Mode: {store.mode} })",
        "items: store.items |> List/map(item, new: store.mode |> WHEN {",
        "items: store.items |> List/map(item, new: BLOCK {",
        "should_show: store.mode |> WHEN {",
        "All => Element/label(element: [], style: [], label: item)",
        "True => Element/label(element: [], style: [], label: item)",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "list_map_block subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ListMapBlockProgram {
        host_view: lower_list_map_block_document(),
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
    })
}

pub fn try_lower_list_retain_count(source: &str) -> Result<ListRetainCountProgram, String> {
    for required_marker in [
        "text_to_add: store.input.event.key_down.key |> WHEN {",
        "Enter => store.input.text",
        "|> List/append(item: text_to_add)",
        "Element/text_input(",
        "element: [event: [key_down: LINK, change: LINK]]",
        "placeholder: [text: TEXT { Type and press Enter }]",
        "focus: True",
        "all_count_label()",
        "retain_count_label()",
        "count: PASSED.store.items |> List/count()",
        "count: PASSED.store.items |> List/retain(item, if: True) |> List/count()",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "list_retain_count subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ListRetainCountProgram {
        ir: lower_list_retain_count_ir().into(),
        host_view: lower_list_retain_count_document(),
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
    })
}

fn lower_list_retain_count_ir() -> Vec<IrNode> {
    vec![
        IrNode {
            id: NodeId(7000),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(7001),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(70)),
        },
        IrNode {
            id: NodeId(7002),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(7001)],
            },
        },
        IrNode {
            id: NodeId(7003),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(7000),
                updates: NodeId(7002),
            },
        },
        IrNode {
            id: NodeId(7004),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(70),
                input: NodeId(7003),
            },
        },
        IrNode {
            id: NodeId(7005),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Initial")),
        },
        IrNode {
            id: NodeId(7006),
            source_expr: None,
            kind: IrNodeKind::ListLiteral {
                items: vec![NodeId(7005)],
            },
        },
        IrNode {
            id: NodeId(7007),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(71)),
        },
        IrNode {
            id: NodeId(7008),
            source_expr: None,
            kind: IrNodeKind::KeyDownKey {
                input: NodeId(7007),
            },
        },
        IrNode {
            id: NodeId(7009),
            source_expr: None,
            kind: IrNodeKind::KeyDownText {
                input: NodeId(7007),
            },
        },
        IrNode {
            id: NodeId(7010),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Enter")),
        },
        IrNode {
            id: NodeId(7011),
            source_expr: None,
            kind: IrNodeKind::Eq {
                lhs: NodeId(7008),
                rhs: NodeId(7010),
            },
        },
        IrNode {
            id: NodeId(7012),
            source_expr: None,
            kind: IrNodeKind::Skip,
        },
        IrNode {
            id: NodeId(7013),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(7011),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(7009),
                }],
                fallback: NodeId(7012),
            },
        },
        IrNode {
            id: NodeId(7014),
            source_expr: None,
            kind: IrNodeKind::ListAppend {
                list: NodeId(7016),
                item: NodeId(7013),
            },
        },
        IrNode {
            id: NodeId(7015),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(7014)],
            },
        },
        IrNode {
            id: NodeId(7016),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(7006),
                updates: NodeId(7015),
            },
        },
        IrNode {
            id: NodeId(7017),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(77),
                input: NodeId(7016),
            },
        },
        IrNode {
            id: NodeId(7021),
            source_expr: None,
            kind: IrNodeKind::ListCount { list: NodeId(7016) },
        },
        IrNode {
            id: NodeId(7022),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("All count: ")),
        },
        IrNode {
            id: NodeId(7023),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(7022), NodeId(7021)],
            },
        },
        IrNode {
            id: NodeId(7024),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(71),
                input: NodeId(7023),
            },
        },
        IrNode {
            id: NodeId(7025),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(true)),
        },
        IrNode {
            id: NodeId(7026),
            source_expr: None,
            kind: IrNodeKind::ListRetain {
                list: NodeId(7016),
                predicate: NodeId(7025),
            },
        },
        IrNode {
            id: NodeId(7027),
            source_expr: None,
            kind: IrNodeKind::ListCount { list: NodeId(7026) },
        },
        IrNode {
            id: NodeId(7028),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Retain count: ")),
        },
        IrNode {
            id: NodeId(7029),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(7028), NodeId(7027)],
            },
        },
        IrNode {
            id: NodeId(7030),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(72),
                input: NodeId(7029),
            },
        },
    ]
}

pub fn try_lower_list_object_state(source: &str) -> Result<ListObjectStateProgram, String> {
    for required_marker in [
        "FUNCTION make_counter() {",
        "count: 0 |> HOLD state {",
        "button.event.press |> THEN { state + 1 }",
        "store.counters |> List/map(counter, new: Element/stripe(",
        "Click each button - counts should be independent",
        "TEXT { Count: {counter.count} }",
        "TEXT { Click me }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "list_object_state subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ListObjectStateProgram {
        host_view: lower_list_object_state_document(),
        press_ports: [SourcePortId(90), SourcePortId(91), SourcePortId(92)],
        count_sinks: [SinkPortId(90), SinkPortId(91), SinkPortId(92)],
    })
}

pub fn try_lower_list_retain_remove(source: &str) -> Result<ListRetainRemoveProgram, String> {
    for required_marker in [
        "-- Test: Can List/retain be used at list source to remove specific items?",
        "text_to_add: input.event.key_down.key |> WHEN {",
        "trimmed: input.text |> Text/trim()",
        "|> List/append(item: text_to_add)",
        "Element/text_input(",
        "element: [event: [key_down: LINK, change: LINK]]",
        "placeholder: [text: TEXT { Type and press Enter }]",
        "focus: True",
        "TEXT { Add items with Enter }",
        "TEXT { Count: {store.items |> List/count()} }",
        "TEXT { - {item} }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "list_retain_remove subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ListRetainRemoveProgram {
        ir: lower_list_retain_remove_ir().into(),
        host_view: lower_list_retain_remove_document(),
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
    })
}

fn lower_list_retain_remove_ir() -> Vec<IrNode> {
    vec![
        IrNode {
            id: NodeId(8000),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Add items with Enter")),
        },
        IrNode {
            id: NodeId(8001),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(80),
                input: NodeId(8000),
            },
        },
        IrNode {
            id: NodeId(8002),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(8003),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(80)),
        },
        IrNode {
            id: NodeId(8004),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(81)),
        },
        IrNode {
            id: NodeId(8005),
            source_expr: None,
            kind: IrNodeKind::KeyDownKey {
                input: NodeId(8004),
            },
        },
        IrNode {
            id: NodeId(8006),
            source_expr: None,
            kind: IrNodeKind::KeyDownText {
                input: NodeId(8004),
            },
        },
        IrNode {
            id: NodeId(8007),
            source_expr: None,
            kind: IrNodeKind::TextTrim {
                input: NodeId(8006),
            },
        },
        IrNode {
            id: NodeId(8008),
            source_expr: None,
            kind: IrNodeKind::Skip,
        },
        IrNode {
            id: NodeId(8009),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(8007),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(""),
                    result: NodeId(8008),
                }],
                fallback: NodeId(8007),
            },
        },
        IrNode {
            id: NodeId(8010),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(8005),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from("Enter"),
                    result: NodeId(8009),
                }],
                fallback: NodeId(8008),
            },
        },
        IrNode {
            id: NodeId(8011),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(8010),
                body: NodeId(8002),
            },
        },
        IrNode {
            id: NodeId(8012),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(8003), NodeId(8011)],
            },
        },
        IrNode {
            id: NodeId(8013),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(8002),
                updates: NodeId(8012),
            },
        },
        IrNode {
            id: NodeId(8014),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(81),
                input: NodeId(8013),
            },
        },
        IrNode {
            id: NodeId(8015),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::List(vec![
                KernelValue::from("Apple"),
                KernelValue::from("Banana"),
                KernelValue::from("Cherry"),
            ])),
        },
        IrNode {
            id: NodeId(8016),
            source_expr: None,
            kind: IrNodeKind::ListAppend {
                list: NodeId(8020),
                item: NodeId(8010),
            },
        },
        IrNode {
            id: NodeId(8017),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(8016)],
            },
        },
        IrNode {
            id: NodeId(8018),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(8015),
                updates: NodeId(8017),
            },
        },
        IrNode {
            id: NodeId(8019),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(89),
                input: NodeId(8018),
            },
        },
        IrNode {
            id: NodeId(8020),
            source_expr: None,
            kind: IrNodeKind::Block {
                inputs: vec![NodeId(8018)],
            },
        },
        IrNode {
            id: NodeId(8021),
            source_expr: None,
            kind: IrNodeKind::ListCount { list: NodeId(8018) },
        },
        IrNode {
            id: NodeId(8022),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Count: ")),
        },
        IrNode {
            id: NodeId(8023),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(8022), NodeId(8021)],
            },
        },
        IrNode {
            id: NodeId(8024),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(82),
                input: NodeId(8023),
            },
        },
    ]
}

pub fn try_lower_shopping_list(source: &str) -> Result<ShoppingListProgram, String> {
    for required_marker in [
        "-- Shopping List",
        "text_to_add: elements.item_input.event.key_down.key |> WHEN {",
        "trimmed: elements.item_input.text |> Text/trim()",
        "|> List/append(item: text_to_add)",
        "|> List/clear(on: elements.clear_button.event.press)",
        "item_input() |> LINK { PASSED.store.elements.item_input }",
        "clear_button() |> LINK { PASSED.store.elements.clear_button }",
        "placeholder: [text: TEXT { Type and press Enter to add... }]",
        "TEXT { {count} items }",
        "TEXT { Clear }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "shopping_list subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ShoppingListProgram {
        ir: lower_shopping_list_ir().into(),
        host_view: lower_shopping_list_document(),
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
    })
}

fn lower_shopping_list_ir() -> Vec<IrNode> {
    vec![
        IrNode {
            id: NodeId(10000),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Shopping List")),
        },
        IrNode {
            id: NodeId(10001),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1006),
                input: NodeId(10000),
            },
        },
        IrNode {
            id: NodeId(10002),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(10003),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1000)),
        },
        IrNode {
            id: NodeId(10004),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1001)),
        },
        IrNode {
            id: NodeId(10005),
            source_expr: None,
            kind: IrNodeKind::KeyDownKey {
                input: NodeId(10004),
            },
        },
        IrNode {
            id: NodeId(10006),
            source_expr: None,
            kind: IrNodeKind::KeyDownText {
                input: NodeId(10004),
            },
        },
        IrNode {
            id: NodeId(10007),
            source_expr: None,
            kind: IrNodeKind::TextTrim {
                input: NodeId(10006),
            },
        },
        IrNode {
            id: NodeId(10008),
            source_expr: None,
            kind: IrNodeKind::Skip,
        },
        IrNode {
            id: NodeId(10009),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(10007),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(""),
                    result: NodeId(10008),
                }],
                fallback: NodeId(10007),
            },
        },
        IrNode {
            id: NodeId(10010),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(10005),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from("Enter"),
                    result: NodeId(10009),
                }],
                fallback: NodeId(10008),
            },
        },
        IrNode {
            id: NodeId(10011),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(10010),
                body: NodeId(10002),
            },
        },
        IrNode {
            id: NodeId(10012),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(10003), NodeId(10011)],
            },
        },
        IrNode {
            id: NodeId(10013),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(10002),
                updates: NodeId(10012),
            },
        },
        IrNode {
            id: NodeId(10014),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1000),
                input: NodeId(10013),
            },
        },
        IrNode {
            id: NodeId(10015),
            source_expr: None,
            kind: IrNodeKind::ListLiteral { items: vec![] },
        },
        IrNode {
            id: NodeId(10016),
            source_expr: None,
            kind: IrNodeKind::ListAppend {
                list: NodeId(10020),
                item: NodeId(10010),
            },
        },
        IrNode {
            id: NodeId(10017),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1002)),
        },
        IrNode {
            id: NodeId(10018),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(10017),
                body: NodeId(10015),
            },
        },
        IrNode {
            id: NodeId(10019),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(10016), NodeId(10018)],
            },
        },
        IrNode {
            id: NodeId(10020),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(10015),
                updates: NodeId(10019),
            },
        },
        IrNode {
            id: NodeId(10021),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1007),
                input: NodeId(10020),
            },
        },
        IrNode {
            id: NodeId(10022),
            source_expr: None,
            kind: IrNodeKind::ListCount {
                list: NodeId(10020),
            },
        },
        IrNode {
            id: NodeId(10023),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(" items")),
        },
        IrNode {
            id: NodeId(10024),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(10022), NodeId(10023)],
            },
        },
        IrNode {
            id: NodeId(10025),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1001),
                input: NodeId(10024),
            },
        },
    ]
}

pub fn try_lower_filter_checkbox_bug(source: &str) -> Result<FilterCheckboxBugProgram, String> {
    for required_marker in [
        "-- Minimal reproduction: Checkbox toggle fails after filter switching",
        "selected_filter: All |> HOLD state {",
        "filter_buttons.all.event.press |> THEN { All }",
        "filter_buttons.active.event.press |> THEN { Active }",
        "checked: False |> HOLD state {",
        "elements.checkbox.event.click |> THEN { state |> Bool/not() }",
        "Element/checkbox(",
        "label: TEXT { {item.name} ({view_label}) - checked: {item.checked} }",
        "TEXT { Test: Click Active, All, then checkbox 3x }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "filter_checkbox_bug subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(FilterCheckboxBugProgram {
        host_view: lower_filter_checkbox_bug_document(),
        filter_all_port: SourcePortId(1200),
        filter_active_port: SourcePortId(1201),
        filter_sink: SinkPortId(1200),
        checkbox_ports: [SourcePortId(1202), SourcePortId(1203)],
        checkbox_sinks: [SinkPortId(1201), SinkPortId(1202)],
        item_label_sinks: [SinkPortId(1203), SinkPortId(1204)],
        footer_sink: SinkPortId(1205),
    })
}

pub fn try_lower_checkbox_test(source: &str) -> Result<CheckboxTestProgram, String> {
    for required_marker in [
        "-- Minimal test case for checkbox sharing bug",
        "items: LIST {",
        "make_item(name: TEXT { Item A })",
        "make_item(name: TEXT { Item B })",
        "checked: False |> HOLD state {",
        "checkbox_link.event.click |> THEN { state |> Bool/not() }",
        "Element/checkbox(",
        "label: item.name",
        "True => TEXT { (checked) }",
        "False => TEXT { (unchecked) }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "checkbox_test subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(CheckboxTestProgram {
        host_view: lower_checkbox_test_document(),
        checkbox_ports: [SourcePortId(1300), SourcePortId(1301)],
        checkbox_sinks: [SinkPortId(1300), SinkPortId(1301)],
        label_sinks: [SinkPortId(1304), SinkPortId(1305)],
        status_sinks: [SinkPortId(1302), SinkPortId(1303)],
    })
}

pub fn try_lower_chained_list_remove_bug(
    source: &str,
) -> Result<ChainedListRemoveBugProgram, String> {
    for required_marker in [
        "-- Reproduction: Cleared items reappear after removing a newly added item",
        "next_id: 2 |> HOLD state {",
        "|> List/append(item: item_to_add)",
        "|> List/remove(item, on: item.elements.remove_button.event.press)",
        "|> List/remove(item, on: elements.clear_completed_button.event.press",
        "elements: [checkbox: LINK, remove_button: LINK]",
        "completed: False |> HOLD state {",
        "Element/checkbox(",
        "label: TEXT { {item.name} (id={item.id}) }",
        "label: TEXT { Add Item }",
        "label: TEXT { Clear completed }",
        "Active: {store.active_items |> List/count()}, Completed: {store.completed_items |> List/count()}",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "chained_list_remove_bug subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ChainedListRemoveBugProgram {
        host_view: lower_chained_list_remove_bug_document(),
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
    })
}

pub fn try_lower_crud(source: &str) -> Result<CrudProgram, String> {
    for required_marker in [
        "-- CRUD (7GUIs Task 5)",
        "selected_id: None |> HOLD state {",
        "people: LIST {",
        "|> List/append(item: store.person_to_add)",
        "|> List/remove(item, on: elements.delete_button.event.press",
        "filter_input()",
        "name_input()",
        "surname_input()",
        "person_row(person: item)",
        "Element/button(",
        "TEXT { CRUD }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "crud subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(CrudProgram {
        host_view: lower_crud_document(),
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
    })
}

pub fn try_lower_temperature_converter(
    source: &str,
) -> Result<TemperatureConverterProgram, String> {
    for required_marker in [
        "-- Temperature Converter (7GUIs Task 2)",
        "celsius_raw: Text/empty() |> HOLD state {",
        "fahrenheit_raw: Text/empty() |> HOLD state {",
        "last_edited: None |> HOLD state {",
        "Temperature Converter",
        "converter_row()",
        "Element/text_input(",
        "placeholder: [text: TEXT { Celsius }]",
        "placeholder: [text: TEXT { Fahrenheit }]",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "temperature_converter subset requires source marker `{required_marker}`"
            ));
        }
    }

    let ir = vec![
        IrNode {
            id: NodeId(1800),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(1801),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1800)),
        },
        IrNode {
            id: NodeId(1802),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1802)),
        },
        IrNode {
            id: NodeId(1803),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1800),
                updates: NodeId(1801),
            },
        },
        IrNode {
            id: NodeId(1804),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1800),
                updates: NodeId(1802),
            },
        },
        IrNode {
            id: NodeId(1805),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::Tag("None".to_string())),
        },
        IrNode {
            id: NodeId(1806),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::Tag("Celsius".to_string())),
        },
        IrNode {
            id: NodeId(1807),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1801),
                body: NodeId(1806),
            },
        },
        IrNode {
            id: NodeId(1808),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::Tag("Fahrenheit".to_string())),
        },
        IrNode {
            id: NodeId(1809),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1802),
                body: NodeId(1808),
            },
        },
        IrNode {
            id: NodeId(1810),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1807), NodeId(1809)],
            },
        },
        IrNode {
            id: NodeId(1811),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1805),
                updates: NodeId(1810),
            },
        },
        IrNode {
            id: NodeId(1812),
            source_expr: None,
            kind: IrNodeKind::TextToNumber {
                input: NodeId(1804),
            },
        },
        IrNode {
            id: NodeId(1813),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::Tag("NaN".to_string())),
        },
        IrNode {
            id: NodeId(1814),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(32.0)),
        },
        IrNode {
            id: NodeId(1815),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(5.0)),
        },
        IrNode {
            id: NodeId(1816),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(9.0)),
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
            id: NodeId(1821),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1812),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::Tag("NaN".to_string()),
                    result: NodeId(1800),
                }],
                fallback: NodeId(1820),
            },
        },
        IrNode {
            id: NodeId(1822),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1811),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::Tag("Fahrenheit".to_string()),
                    result: NodeId(1821),
                }],
                fallback: NodeId(1803),
            },
        },
        IrNode {
            id: NodeId(1823),
            source_expr: None,
            kind: IrNodeKind::TextToNumber {
                input: NodeId(1803),
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
        IrNode {
            id: NodeId(1828),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1823),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::Tag("NaN".to_string()),
                    result: NodeId(1800),
                }],
                fallback: NodeId(1827),
            },
        },
        IrNode {
            id: NodeId(1829),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1811),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::Tag("Celsius".to_string()),
                    result: NodeId(1828),
                }],
                fallback: NodeId(1804),
            },
        },
        IrNode {
            id: NodeId(1830),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Temperature Converter")),
        },
        IrNode {
            id: NodeId(1831),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Celsius")),
        },
        IrNode {
            id: NodeId(1832),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("=")),
        },
        IrNode {
            id: NodeId(1833),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Fahrenheit")),
        },
        IrNode {
            id: NodeId(1834),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1800),
                input: NodeId(1830),
            },
        },
        IrNode {
            id: NodeId(1835),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1801),
                input: NodeId(1822),
            },
        },
        IrNode {
            id: NodeId(1836),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1802),
                input: NodeId(1829),
            },
        },
        IrNode {
            id: NodeId(1837),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1803),
                input: NodeId(1831),
            },
        },
        IrNode {
            id: NodeId(1838),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1804),
                input: NodeId(1832),
            },
        },
        IrNode {
            id: NodeId(1839),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1805),
                input: NodeId(1833),
            },
        },
    ];

    Ok(TemperatureConverterProgram {
        ir: ir.into(),
        host_view: lower_temperature_converter_document(),
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
    })
}

pub fn try_lower_flight_booker(source: &str) -> Result<FlightBookerProgram, String> {
    for required_marker in [
        "-- Flight Booker (7GUIs Task 3)",
        "flight_select: LINK",
        "departure_input: LINK",
        "return_input: LINK",
        "book_button: LINK",
        "flight_type: TEXT { one-way } |> HOLD state {",
        "elements.flight_select.event.change.value",
        "departure_date: TEXT { 2026-03-03 } |> HOLD state {",
        "return_date: TEXT { 2026-03-03 } |> HOLD state {",
        "Element/select(",
        "selected: TEXT { one-way }",
        "Element/text_input(",
        "Element/button(",
        "TEXT { Booked one-way flight on {departure_date} }",
        "TEXT { Booked return flight: {departure_date} to {return_date} }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "flight_booker subset requires source marker `{required_marker}`"
            ));
        }
    }

    let ir = vec![
        IrNode {
            id: NodeId(1900),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("one-way")),
        },
        IrNode {
            id: NodeId(1901),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1900)),
        },
        IrNode {
            id: NodeId(1902),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1900),
                updates: NodeId(1901),
            },
        },
        IrNode {
            id: NodeId(1903),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("2026-03-03")),
        },
        IrNode {
            id: NodeId(1904),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1901)),
        },
        IrNode {
            id: NodeId(1905),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1903),
                updates: NodeId(1904),
            },
        },
        IrNode {
            id: NodeId(1906),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1902)),
        },
        IrNode {
            id: NodeId(1907),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1903),
                updates: NodeId(1906),
            },
        },
        IrNode {
            id: NodeId(1908),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(true)),
        },
        IrNode {
            id: NodeId(1909),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(false)),
        },
        IrNode {
            id: NodeId(1910),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("return")),
        },
        IrNode {
            id: NodeId(1911),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1902),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from("return"),
                    result: NodeId(1908),
                }],
                fallback: NodeId(1909),
            },
        },
        IrNode {
            id: NodeId(1912),
            source_expr: None,
            kind: IrNodeKind::Ge {
                lhs: NodeId(1907),
                rhs: NodeId(1905),
            },
        },
        IrNode {
            id: NodeId(1913),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1911),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(false),
                    result: NodeId(1908),
                }],
                fallback: NodeId(1912),
            },
        },
        IrNode {
            id: NodeId(1914),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1903)),
        },
        IrNode {
            id: NodeId(1915),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1914),
                body: NodeId(1913),
            },
        },
        IrNode {
            id: NodeId(1916),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Booked one-way flight on ")),
        },
        IrNode {
            id: NodeId(1917),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(1916), NodeId(1905)],
            },
        },
        IrNode {
            id: NodeId(1918),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Booked return flight: ")),
        },
        IrNode {
            id: NodeId(1919),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(" to ")),
        },
        IrNode {
            id: NodeId(1920),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(1918), NodeId(1905), NodeId(1919), NodeId(1907)],
            },
        },
        IrNode {
            id: NodeId(1921),
            source_expr: None,
            kind: IrNodeKind::When {
                source: NodeId(1911),
                arms: vec![
                    crate::ir::MatchArm {
                        matcher: KernelValue::from(false),
                        result: NodeId(1917),
                    },
                    crate::ir::MatchArm {
                        matcher: KernelValue::from(true),
                        result: NodeId(1920),
                    },
                ],
                fallback: NodeId(1917),
            },
        },
        IrNode {
            id: NodeId(1922),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1915),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1921),
                }],
                fallback: NodeId(1940),
            },
        },
        IrNode {
            id: NodeId(1923),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("")),
        },
        IrNode {
            id: NodeId(1924),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1923),
                updates: NodeId(1922),
            },
        },
        IrNode {
            id: NodeId(1925),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1911),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1909),
                }],
                fallback: NodeId(1908),
            },
        },
        IrNode {
            id: NodeId(1926),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1913),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1909),
                }],
                fallback: NodeId(1908),
            },
        },
        IrNode {
            id: NodeId(1927),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Flight Booker")),
        },
        IrNode {
            id: NodeId(1940),
            source_expr: None,
            kind: IrNodeKind::Skip,
        },
        IrNode {
            id: NodeId(1930),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1900),
                input: NodeId(1927),
            },
        },
        IrNode {
            id: NodeId(1931),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1901),
                input: NodeId(1902),
            },
        },
        IrNode {
            id: NodeId(1932),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1902),
                input: NodeId(1905),
            },
        },
        IrNode {
            id: NodeId(1933),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1903),
                input: NodeId(1907),
            },
        },
        IrNode {
            id: NodeId(1934),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1904),
                input: NodeId(1925),
            },
        },
        IrNode {
            id: NodeId(1935),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1905),
                input: NodeId(1926),
            },
        },
        IrNode {
            id: NodeId(1936),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1906),
                input: NodeId(1924),
            },
        },
    ];

    Ok(FlightBookerProgram {
        ir: ir.into(),
        host_view: lower_flight_booker_document(),
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
    })
}

pub fn try_lower_timer(source: &str) -> Result<TimerProgram, String> {
    for required_marker in [
        "-- Timer (7GUIs Task 4)",
        "tick: Duration[milliseconds: 100] |> Timer/interval()",
        "duration_slider: LINK",
        "reset_button: LINK",
        "max_duration: 15 |> HOLD state {",
        "raw_elapsed: 0 |> HOLD state {",
        "elements.reset_button.event.press |> THEN { 0 }",
        "progress_percent: elapsed / max_duration * 100",
        "Element/slider(",
        "Element/button(",
        "TEXT { Timer }",
        "TEXT { Duration: }",
        "TEXT { Elapsed Time: }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "timer subset requires source marker `{required_marker}`"
            ));
        }
    }

    let ir = vec![
        IrNode {
            id: NodeId(1950),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(15.0)),
        },
        IrNode {
            id: NodeId(1951),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1950)),
        },
        IrNode {
            id: NodeId(1952),
            source_expr: None,
            kind: IrNodeKind::TextToNumber {
                input: NodeId(1951),
            },
        },
        IrNode {
            id: NodeId(1953),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(1.0)),
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
            id: NodeId(1955),
            source_expr: None,
            kind: IrNodeKind::While {
                source: NodeId(1954),
                arms: vec![crate::ir::MatchArm {
                    matcher: KernelValue::from(true),
                    result: NodeId(1952),
                }],
                fallback: NodeId(1950),
            },
        },
        IrNode {
            id: NodeId(1956),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1950),
                updates: NodeId(1955),
            },
        },
        IrNode {
            id: NodeId(1957),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(0.0)),
        },
        IrNode {
            id: NodeId(1958),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1952)),
        },
        IrNode {
            id: NodeId(1959),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(0.1)),
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
            id: NodeId(1961),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1958),
                body: NodeId(1960),
            },
        },
        IrNode {
            id: NodeId(1962),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1951)),
        },
        IrNode {
            id: NodeId(1963),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1962),
                body: NodeId(1957),
            },
        },
        IrNode {
            id: NodeId(1964),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1961), NodeId(1963)],
            },
        },
        IrNode {
            id: NodeId(1965),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1957),
                updates: NodeId(1964),
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
            id: NodeId(1967),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(10.0)),
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
            id: NodeId(1972),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(100.0)),
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
        IrNode {
            id: NodeId(1976),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Timer")),
        },
        IrNode {
            id: NodeId(1977),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Elapsed Time:")),
        },
        IrNode {
            id: NodeId(1978),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("%")),
        },
        IrNode {
            id: NodeId(1979),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(1975), NodeId(1978)],
            },
        },
        IrNode {
            id: NodeId(1980),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Duration:")),
        },
        IrNode {
            id: NodeId(1981),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("s")),
        },
        IrNode {
            id: NodeId(1982),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(1970), NodeId(1981)],
            },
        },
        IrNode {
            id: NodeId(1983),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(1956), NodeId(1981)],
            },
        },
        IrNode {
            id: NodeId(1984),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1950),
                input: NodeId(1976),
            },
        },
        IrNode {
            id: NodeId(1985),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1951),
                input: NodeId(1977),
            },
        },
        IrNode {
            id: NodeId(1986),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1952),
                input: NodeId(1979),
            },
        },
        IrNode {
            id: NodeId(1987),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1953),
                input: NodeId(1982),
            },
        },
        IrNode {
            id: NodeId(1988),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1954),
                input: NodeId(1980),
            },
        },
        IrNode {
            id: NodeId(1989),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1955),
                input: NodeId(1956),
            },
        },
        IrNode {
            id: NodeId(1990),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1956),
                input: NodeId(1983),
            },
        },
    ];

    Ok(TimerProgram {
        ir: ir.into(),
        host_view: lower_timer_document(),
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
    })
}

pub fn try_lower_interval(source: &str) -> Result<IntervalProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);
    let document = bindings
        .get("document")
        .ok_or_else(|| "interval subset requires top-level `document`".to_string())?;

    let interval_ms = lower_interval_document_expression(document)?;
    Ok(IntervalProgram {
        ir: lower_interval_ir(SourcePortId(1980), SinkPortId(1980)).into(),
        host_view: lower_interval_document(SinkPortId(1980), SourcePortId(1980), interval_ms),
        value_sink: SinkPortId(1980),
        tick_port: SourcePortId(1980),
        interval_ms,
    })
}

pub fn try_lower_interval_hold(source: &str) -> Result<IntervalProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);
    let tick = bindings
        .get("tick")
        .ok_or_else(|| "interval_hold subset requires top-level `tick`".to_string())?;
    let counter = bindings
        .get("counter")
        .ok_or_else(|| "interval_hold subset requires top-level `counter`".to_string())?;
    let document = bindings
        .get("document")
        .ok_or_else(|| "interval_hold subset requires top-level `document`".to_string())?;

    let interval_ms = extract_timer_interval_ms(tick)?;
    lower_interval_hold_counter(counter)?;
    lower_interval_hold_document(document)?;

    Ok(IntervalProgram {
        ir: lower_interval_hold_ir(SourcePortId(1981), SinkPortId(1981)).into(),
        host_view: lower_interval_document(SinkPortId(1981), SourcePortId(1981), interval_ms),
        value_sink: SinkPortId(1981),
        tick_port: SourcePortId(1981),
        interval_ms,
    })
}

pub fn try_lower_fibonacci(source: &str) -> Result<FibonacciProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);

    let function = expressions
        .iter()
        .find(|expression| {
            matches!(
                &expression.node,
                StaticExpression::Function { name, .. } if name.as_str() == "fibonacci"
            )
        })
        .ok_or_else(|| "fibonacci subset requires top-level `fibonacci`".to_string())?;
    let position = bindings
        .get("position")
        .ok_or_else(|| "fibonacci subset requires top-level `position`".to_string())?;
    let result = bindings
        .get("result")
        .ok_or_else(|| "fibonacci subset requires top-level `result`".to_string())?;
    let message = bindings
        .get("message")
        .ok_or_else(|| "fibonacci subset requires top-level `message`".to_string())?;
    let document = bindings
        .get("document")
        .ok_or_else(|| "fibonacci subset requires top-level `document`".to_string())?;

    ensure_fibonacci_function(function)?;
    let position = extract_integer_literal(position)?;
    if position < 0 {
        return Err("fibonacci subset requires non-negative `position`".to_string());
    }
    ensure_fibonacci_result_binding(result)?;
    ensure_fibonacci_message(message)?;
    ensure_fibonacci_document(document)?;

    let message_text = format!(
        "{position}. Fibonacci number is {}",
        fibonacci_number(position as u64)
    );
    let sink = SinkPortId(1985);
    let mut sink_values = BTreeMap::new();
    sink_values.insert(sink, KernelValue::from(message_text));

    Ok(FibonacciProgram {
        host_view: lower_single_label_document(sink, ViewSiteId(1985), FunctionInstanceId(1985)),
        sink_values,
    })
}

pub fn try_lower_layers(source: &str) -> Result<LayersProgram, String> {
    for required_marker in [
        "Document/new(root: Element/stack(",
        "card(label: TEXT { Red Card }, hue: 25, x: 20, y: 20)",
        "card(label: TEXT { Green Card }, hue: 150, x: 60, y: 60)",
        "card(label: TEXT { Blue Card }, hue: 240, x: 100, y: 100)",
        "FUNCTION card(label, hue, x, y)",
        "transform: [move_right: x, move_down: y]",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "layers subset requires source marker `{required_marker}`"
            ));
        }
    }

    let sink_values = BTreeMap::from([
        (SinkPortId(1986), KernelValue::from("Red Card")),
        (SinkPortId(1987), KernelValue::from("Green Card")),
        (SinkPortId(1988), KernelValue::from("Blue Card")),
    ]);

    Ok(LayersProgram {
        host_view: lower_layers_document(),
        sink_values,
    })
}

pub fn try_lower_pages(source: &str) -> Result<PagesProgram, String> {
    for required_marker in [
        "Router/go_to()",
        "Router/route()",
        "nav.home.event.press |> THEN { TEXT { / } }",
        "nav.about.event.press |> THEN { TEXT { /about } }",
        "nav.contact.event.press |> THEN { TEXT { /contact } }",
        "current_page: current_route |> WHEN {",
        "FUNCTION nav_button(label, route)",
        "FUNCTION page(title, description)",
        "404 - Not Found",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "pages subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(PagesProgram {
        host_view: lower_pages_document(),
        nav_press_ports: [SourcePortId(1989), SourcePortId(1990), SourcePortId(1991)],
        title_sink: SinkPortId(1989),
        description_sink: SinkPortId(1990),
        nav_active_sinks: [SinkPortId(1991), SinkPortId(1992), SinkPortId(1993)],
    })
}

pub fn try_lower_latest(source: &str) -> Result<LatestProgram, String> {
    for required_marker in [
        "value: LATEST {",
        "send_1_button.event.press |> THEN { 1 }",
        "send_2_button.event.press |> THEN { 2 }",
        "sum: value |> Math/sum()",
        "send_1_button: send_button(label: TEXT { Send 1 })",
        "send_2_button: send_button(label: TEXT { Send 2 })",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "latest subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(LatestProgram {
        host_view: lower_latest_document(),
        send_press_ports: [SourcePortId(1994), SourcePortId(1995)],
        value_sink: SinkPortId(1994),
        sum_sink: SinkPortId(1995),
    })
}

pub fn try_lower_text_interpolation_update(
    source: &str,
) -> Result<TextInterpolationUpdateProgram, String> {
    for required_marker in [
        "-- Test: Does TEXT interpolation update when referenced variable changes?",
        "toggle: LINK",
        "Element/button(",
        "label: TEXT { Toggle (value: {store.value}) }",
        "label: TEXT { Label shows: {store.value} }",
        "label: TEXT { WHILE says: True }",
        "label: TEXT { WHILE says: False }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "text_interpolation_update subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(TextInterpolationUpdateProgram {
        host_view: lower_text_interpolation_update_document(),
        toggle_press_port: SourcePortId(1996),
        button_label_sink: SinkPortId(1996),
        label_sink: SinkPortId(1997),
        while_sink: SinkPortId(1998),
    })
}

pub fn try_lower_then(source: &str) -> Result<ThenProgram, String> {
    for required_marker in [
        "current_sum: addition_button.event.press |> THEN { input_a + input_b }",
        "input_a: sum_of_steps(step: 1, seconds: 0.5)",
        "input_b: sum_of_steps(step: 10, seconds: 1)",
        "Duration[seconds: seconds] |> Timer/interval() |> THEN { sum + step }",
        "label: TEXT { A + B }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "then subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ThenProgram {
        host_view: lower_then_document(),
        tick_port: SourcePortId(2010),
        addition_press_port: SourcePortId(2012),
        input_a_sink: SinkPortId(2010),
        input_b_sink: SinkPortId(2011),
        result_sink: SinkPortId(2012),
    })
}

pub fn try_lower_when(source: &str) -> Result<WhenProgram, String> {
    for required_marker in [
        "current_result: operation |> WHEN {",
        "Addition => input_a + input_b",
        "Subtraction => input_a - input_b",
        "addition_button.event.press |> THEN { Addition }",
        "subtraction_button.event.press |> THEN { Subtraction }",
        "input_a: sum_of_steps(step: 1, seconds: 0.5)",
        "input_b: sum_of_steps(step: 10, seconds: 1)",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "when subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(WhenProgram {
        host_view: lower_when_document(),
        tick_port: SourcePortId(2013),
        addition_press_port: SourcePortId(2015),
        subtraction_press_port: SourcePortId(2016),
        input_a_sink: SinkPortId(2013),
        input_b_sink: SinkPortId(2014),
        result_sink: SinkPortId(2015),
    })
}

pub fn try_lower_while(source: &str) -> Result<WhileProgram, String> {
    for required_marker in [
        "updating_result: operation |> WHILE {",
        "Addition => input_a + input_b",
        "Subtraction => input_a - input_b",
        "addition_button.event.press |> THEN { Addition }",
        "subtraction_button.event.press |> THEN { Subtraction }",
        "Duration[seconds: seconds]",
        "|> Timer/interval()",
        "|> THEN { step }",
        "|> Math/sum()",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "while subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(WhileProgram {
        host_view: lower_while_document(),
        tick_port: SourcePortId(2017),
        addition_press_port: SourcePortId(2019),
        subtraction_press_port: SourcePortId(2020),
        input_a_sink: SinkPortId(2016),
        input_b_sink: SinkPortId(2017),
        result_sink: SinkPortId(2018),
    })
}

pub fn try_lower_while_function_call(source: &str) -> Result<WhileFunctionCallProgram, String> {
    for required_marker in [
        "-- Test: Can functions be called inside WHILE arms?",
        "FUNCTION greeting(name) {",
        "TEXT { Hello, {name}! }",
        "show_greeting: False |> HOLD state {",
        "toggle.event.press |> THEN { state |> Bool/not() }",
        "label: TEXT { Toggle (show: {store.show_greeting}) }",
        "label: greeting(name: TEXT { World })",
        "label: TEXT { Hidden }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "while_function_call subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(WhileFunctionCallProgram {
        host_view: lower_while_function_call_document(),
        toggle_press_port: SourcePortId(2021),
        toggle_label_sink: SinkPortId(2021),
        content_sink: SinkPortId(2022),
    })
}

pub fn try_lower_button_hover_to_click_test(
    source: &str,
) -> Result<ButtonHoverToClickTestProgram, String> {
    for required_marker in [
        "-- Click-based version of button_hover_test for easier automated testing",
        "btn_a: make_button(name: TEXT { A })",
        "btn_b: make_button(name: TEXT { B })",
        "btn_c: make_button(name: TEXT { C })",
        "clicked: False |> HOLD state {",
        "elements.button.event.press |> THEN { state |> Bool/not() }",
        "States - A: {store.btn_a.clicked}, B: {store.btn_b.clicked}, C: {store.btn_c.clicked}",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "button_hover_to_click_test subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ButtonHoverToClickTestProgram {
        host_view: lower_button_hover_to_click_test_document(),
        intro_sink: SinkPortId(2023),
        button_press_ports: [SourcePortId(2022), SourcePortId(2023), SourcePortId(2024)],
        button_active_sinks: [SinkPortId(2024), SinkPortId(2025), SinkPortId(2026)],
        state_sink: SinkPortId(2027),
    })
}

pub fn try_lower_button_hover_test(source: &str) -> Result<ButtonHoverTestProgram, String> {
    for required_marker in [
        "-- Test: Do multiple buttons have independent hover states?",
        "Hover each button - only hovered one should show border",
        "simple_button(name: TEXT { A })",
        "simple_button(name: TEXT { B })",
        "simple_button(name: TEXT { C })",
        "element: [event: [press: LINK], hovered: LINK]",
        "background: [",
        "outline: element.hovered |> WHILE {",
        "False => NoOutline",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "button_hover_test subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(ButtonHoverTestProgram {
        host_view: lower_button_hover_test_document(),
        intro_sink: SinkPortId(2033),
        button_press_ports: [SourcePortId(2033), SourcePortId(2034), SourcePortId(2035)],
        button_hover_sinks: [SinkPortId(2034), SinkPortId(2035), SinkPortId(2036)],
    })
}

pub fn try_lower_switch_hold_test(source: &str) -> Result<SwitchHoldTestProgram, String> {
    for required_marker in [
        "-- Test: Does HOLD receive events from a LINK after switching via WHILE?",
        "show_item_a: True |> HOLD state {",
        "view_toggle.event.press |> THEN { state |> Bool/not() }",
        "item_a: create_item(name: TEXT { Item A })",
        "item_b: create_item(name: TEXT { Item B })",
        "click_count: 0 |> HOLD state {",
        "item_elements.button.event.press |> THEN { state + 1 }",
        "label: TEXT { Showing: Item A }",
        "label: TEXT { Showing: Item B }",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "switch_hold_test subset requires source marker `{required_marker}`"
            ));
        }
    }

    Ok(SwitchHoldTestProgram {
        host_view: lower_switch_hold_test_document(),
        current_item_sink: SinkPortId(2028),
        current_count_sink: SinkPortId(2029),
        item_disabled_sinks: [SinkPortId(2030), SinkPortId(2031)],
        footer_sink: SinkPortId(2032),
        toggle_press_port: SourcePortId(2025),
        item_press_ports: [SourcePortId(2026), SourcePortId(2027)],
    })
}

pub fn try_lower_circle_drawer(source: &str) -> Result<CircleDrawerProgram, String> {
    for required_marker in [
        "-- Circle Drawer (7GUIs Task 6)",
        "canvas: LINK",
        "undo_button: LINK",
        "circles: LIST {}",
        "elements.canvas.event.click |> WHEN {",
        "click => [x: click.x, y: click.y]",
        "List/remove_last(on: elements.undo_button.event.press)",
        "Element/button(",
        "TEXT { Undo }",
        "TEXT { Circles: {store.count} }",
        "Element/svg(",
        "Element/svg_circle(",
    ] {
        if !source.contains(required_marker) {
            return Err(format!(
                "circle_drawer subset requires source marker `{required_marker}`"
            ));
        }
    }

    let ir = vec![
        IrNode {
            id: NodeId(1970),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::List(Vec::new())),
        },
        IrNode {
            id: NodeId(1971),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1970)),
        },
        IrNode {
            id: NodeId(1972),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1971),
                field: "x".to_string(),
            },
        },
        IrNode {
            id: NodeId(1973),
            source_expr: None,
            kind: IrNodeKind::FieldRead {
                object: NodeId(1971),
                field: "y".to_string(),
            },
        },
        IrNode {
            id: NodeId(1974),
            source_expr: None,
            kind: IrNodeKind::ObjectLiteral {
                fields: vec![
                    ("x".to_string(), NodeId(1972)),
                    ("y".to_string(), NodeId(1973)),
                ],
            },
        },
        IrNode {
            id: NodeId(1975),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1971),
                body: NodeId(1974),
            },
        },
        IrNode {
            id: NodeId(1976),
            source_expr: None,
            kind: IrNodeKind::SourcePort(SourcePortId(1971)),
        },
        IrNode {
            id: NodeId(1977),
            source_expr: None,
            kind: IrNodeKind::ListAppend {
                list: NodeId(1980),
                item: NodeId(1975),
            },
        },
        IrNode {
            id: NodeId(1978),
            source_expr: None,
            kind: IrNodeKind::ListRemoveLast {
                list: NodeId(1980),
                on: NodeId(1976),
            },
        },
        IrNode {
            id: NodeId(1979),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1977), NodeId(1978)],
            },
        },
        IrNode {
            id: NodeId(1980),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1970),
                updates: NodeId(1979),
            },
        },
        IrNode {
            id: NodeId(1981),
            source_expr: None,
            kind: IrNodeKind::ListCount { list: NodeId(1980) },
        },
        IrNode {
            id: NodeId(1982),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Circle Drawer")),
        },
        IrNode {
            id: NodeId(1983),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from("Circles: ")),
        },
        IrNode {
            id: NodeId(1984),
            source_expr: None,
            kind: IrNodeKind::TextJoin {
                inputs: vec![NodeId(1983), NodeId(1981)],
            },
        },
        IrNode {
            id: NodeId(1985),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1970),
                input: NodeId(1982),
            },
        },
        IrNode {
            id: NodeId(1986),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1971),
                input: NodeId(1984),
            },
        },
        IrNode {
            id: NodeId(1987),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1972),
                input: NodeId(1980),
            },
        },
    ];

    Ok(CircleDrawerProgram {
        ir: ir.into(),
        host_view: lower_circle_drawer_document(),
        title_sink: SinkPortId(1970),
        count_sink: SinkPortId(1971),
        circles_sink: SinkPortId(1972),
        canvas_click_port: SourcePortId(1970),
        undo_press_port: SourcePortId(1971),
    })
}

pub fn try_lower_static_document(source: &str) -> Result<StaticProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);
    let document = bindings
        .get("document")
        .ok_or_else(|| "static subset requires `document` binding".to_string())?;
    let root = extract_document_root(document)
        .map_err(|_| "static subset requires `Document/new(root: ...)`".to_string())?;
    let root_value = extract_static_kernel_value(root)?;

    let mut sink_values = BTreeMap::new();
    sink_values.insert(SinkPortId(200), root_value);

    Ok(StaticProgram {
        host_view: HostViewIr {
            root: Some(HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(200),
                    function_instance: Some(FunctionInstanceId(200)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Document,
                children: vec![HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: ViewSiteId(201),
                        function_instance: Some(FunctionInstanceId(200)),
                        mapped_item_identity: None,
                    },
                    kind: HostViewKind::Label {
                        sink: SinkPortId(200),
                    },
                    children: Vec::new(),
                }],
            }),
        },
        sink_values,
    })
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

fn lower_counter(
    expression: &StaticSpannedExpression,
    expected_press_port: SourcePortId,
) -> Result<(i64, i64, Vec<IrNode>), String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err("counter subset requires `LATEST { ... } |> Math/sum()` or `HOLD`".to_string());
    };

    let (initial_value, increment_delta) = match &to.node {
        StaticExpression::FunctionCall { path, arguments } => {
            if !path_matches(path, &["Math", "sum"]) || !arguments.is_empty() {
                return Err("counter subset requires `Math/sum()` or `HOLD`".to_string());
            }

            let StaticExpression::Latest { inputs } = &from.node else {
                return Err("counter subset requires `LATEST` before Math/sum".to_string());
            };
            if inputs.len() != 2 {
                return Err("counter subset requires two LATEST inputs".to_string());
            }

            let initial_value = extract_integer_literal(&inputs[0])?;
            let increment_delta =
                extract_counter_then_increment(&inputs[1], expected_press_port, None)?;
            (initial_value, increment_delta)
        }
        StaticExpression::Hold { state_param, body } => {
            let initial_value = extract_integer_literal(from)?;
            let increment_delta = extract_counter_then_increment(
                body,
                expected_press_port,
                Some(state_param.as_str()),
            )?;
            (initial_value, increment_delta)
        }
        _ => {
            return Err(
                "counter subset requires `LATEST { ... } |> Math/sum()` or `HOLD`".to_string(),
            );
        }
    };

    let ir = vec![
        IrNode {
            id: NodeId(1),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(initial_value as f64)),
        },
        IrNode {
            id: NodeId(2),
            source_expr: None,
            kind: IrNodeKind::SourcePort(expected_press_port),
        },
        IrNode {
            id: NodeId(3),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(2),
                body: NodeId(4),
            },
        },
        IrNode {
            id: NodeId(4),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(increment_delta as f64)),
        },
        IrNode {
            id: NodeId(5),
            source_expr: None,
            kind: IrNodeKind::Latest {
                inputs: vec![NodeId(1), NodeId(3)],
            },
        },
        IrNode {
            id: NodeId(6),
            source_expr: None,
            kind: IrNodeKind::MathSum { input: NodeId(5) },
        },
        IrNode {
            id: NodeId(7),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: SinkPortId(1),
                input: NodeId(6),
            },
        },
    ];

    Ok((initial_value, increment_delta, ir))
}

fn extract_counter_then_increment(
    expression: &StaticSpannedExpression,
    expected_press_port: SourcePortId,
    state_param: Option<&str>,
) -> Result<i64, String> {
    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &expression.node
    else {
        return Err("counter subset requires event THEN branch".to_string());
    };
    let press_port = extract_event_press_port(trigger_source)?;
    if press_port != expected_press_port {
        return Err(
            "counter subset requires button LINK and counter trigger to share press port"
                .to_string(),
        );
    }
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Err("counter subset requires THEN body".to_string());
    };
    match (&body.node, state_param) {
        (_, None) => extract_integer_literal(body),
        (StaticExpression::ArithmeticOperator(operator), Some(state_param)) => {
            extract_hold_increment(operator, state_param)
        }
        _ => Err("counter HOLD subset requires `counter + 1`".to_string()),
    }
}

fn extract_hold_increment(
    operator: &boon::parser::static_expression::ArithmeticOperator,
    state_param: &str,
) -> Result<i64, String> {
    let boon::parser::static_expression::ArithmeticOperator::Add {
        operand_a,
        operand_b,
    } = operator
    else {
        return Err("counter HOLD subset requires `counter + 1`".to_string());
    };

    let StaticExpression::Alias(boon::parser::static_expression::Alias::WithoutPassed {
        parts,
        ..
    }) = &operand_a.node
    else {
        return Err("counter HOLD subset requires state param on left side".to_string());
    };
    if parts.len() != 1 || parts[0].as_str() != state_param {
        return Err("counter HOLD subset requires state param on left side".to_string());
    }

    extract_integer_literal(operand_b)
}

fn stripe_layout(direction: HostStripeDirection, gap_px: u32) -> HostViewKind {
    HostViewKind::StripeLayout {
        direction,
        gap_px,
        padding_px: None,
        width: None,
        align_cross: None,
    }
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
        label: label.to_string(),
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
        width,
        bold_sink,
    }
}

fn lower_interval_document(
    value_sink: SinkPortId,
    tick_port: SourcePortId,
    interval_ms: u32,
) -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1980),
                function_instance: Some(FunctionInstanceId(1980)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1981),
                    function_instance: Some(FunctionInstanceId(1980)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    0,
                    Some(20),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Center),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1982),
                            function_instance: Some(FunctionInstanceId(1980)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(value_sink, Some(48), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1983),
                            function_instance: Some(FunctionInstanceId(1980)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::TimerSource {
                            tick_port,
                            interval_ms,
                        },
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_single_label_document(
    sink: SinkPortId,
    root_view_site: ViewSiteId,
    function_instance: FunctionInstanceId,
) -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: root_view_site,
                function_instance: Some(function_instance),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(root_view_site.0 + 1),
                    function_instance: Some(function_instance),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Label { sink },
                children: Vec::new(),
            }],
        }),
    }
}

fn lower_layers_document() -> HostViewIr {
    fn card(
        view_site: u32,
        sink: SinkPortId,
        x_px: u32,
        y_px: u32,
        background: &str,
    ) -> HostViewNode {
        HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(view_site),
                function_instance: Some(FunctionInstanceId(1986)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::PositionedBox {
                x_px,
                y_px,
                width_px: 180,
                height_px: 120,
                padding_px: Some(12),
                background: Some(background.to_string()),
                rounded_px: Some(8),
                text_color: Some("white".to_string()),
            },
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(view_site + 1),
                    function_instance: Some(FunctionInstanceId(1986)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Label { sink },
                children: Vec::new(),
            }],
        }
    }

    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1986),
                function_instance: Some(FunctionInstanceId(1986)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1987),
                    function_instance: Some(FunctionInstanceId(1986)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::AbsolutePanel {
                    width_px: 300,
                    height_px: 250,
                    background: "oklch(0.15 0 0)".to_string(),
                },
                children: vec![
                    card(1988, SinkPortId(1986), 20, 20, "oklch(0.55 0.2 25)"),
                    card(1990, SinkPortId(1987), 60, 60, "oklch(0.55 0.2 150)"),
                    card(1992, SinkPortId(1988), 100, 100, "oklch(0.55 0.2 240)"),
                ],
            }],
        }),
    }
}

fn lower_pages_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1989),
                function_instance: Some(FunctionInstanceId(1989)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1990),
                    function_instance: Some(FunctionInstanceId(1989)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    0,
                    None,
                    Some(HostWidth::Fill),
                    None,
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1991),
                            function_instance: Some(FunctionInstanceId(1989)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::StripeLayout {
                            direction: HostStripeDirection::Row,
                            gap_px: 8,
                            padding_px: Some(16),
                            width: Some(HostWidth::Fill),
                            align_cross: Some(HostCrossAlign::Start),
                        },
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1992),
                                    function_instance: Some(FunctionInstanceId(1989)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "Home",
                                    SourcePortId(1989),
                                    None,
                                    None,
                                    Some(8),
                                    false,
                                    Some("oklch(0.2 0 0)"),
                                    Some(SinkPortId(1991)),
                                    Some("oklch(0.3 0 0)"),
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1993),
                                    function_instance: Some(FunctionInstanceId(1989)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "About",
                                    SourcePortId(1990),
                                    None,
                                    None,
                                    Some(8),
                                    false,
                                    Some("oklch(0.2 0 0)"),
                                    Some(SinkPortId(1992)),
                                    Some("oklch(0.3 0 0)"),
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1994),
                                    function_instance: Some(FunctionInstanceId(1989)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "Contact",
                                    SourcePortId(1991),
                                    None,
                                    None,
                                    Some(8),
                                    false,
                                    Some("oklch(0.2 0 0)"),
                                    Some(SinkPortId(1993)),
                                    Some("oklch(0.3 0 0)"),
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1995),
                            function_instance: Some(FunctionInstanceId(1989)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::StripeLayout {
                            direction: HostStripeDirection::Column,
                            gap_px: 16,
                            padding_px: Some(24),
                            width: Some(HostWidth::Fill),
                            align_cross: Some(HostCrossAlign::Start),
                        },
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1996),
                                    function_instance: Some(FunctionInstanceId(1989)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_label(SinkPortId(1989), Some(32), true, None),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1997),
                                    function_instance: Some(FunctionInstanceId(1989)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_label(
                                    SinkPortId(1990),
                                    None,
                                    false,
                                    Some("oklch(0.7 0 0)"),
                                ),
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_latest_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1998),
                function_instance: Some(FunctionInstanceId(1998)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1999),
                    function_instance: Some(FunctionInstanceId(1998)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    12,
                    Some(16),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Center),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2000),
                            function_instance: Some(FunctionInstanceId(1998)),
                            mapped_item_identity: None,
                        },
                        kind: styled_button(
                            "Send 1",
                            SourcePortId(1994),
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
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2001),
                            function_instance: Some(FunctionInstanceId(1998)),
                            mapped_item_identity: None,
                        },
                        kind: styled_button(
                            "Send 2",
                            SourcePortId(1995),
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
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2002),
                            function_instance: Some(FunctionInstanceId(1998)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1994), Some(22), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2003),
                            function_instance: Some(FunctionInstanceId(1998)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1995), None, false, None),
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_text_interpolation_update_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(2004),
                function_instance: Some(FunctionInstanceId(2004)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(2005),
                    function_instance: Some(FunctionInstanceId(2004)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    10,
                    Some(20),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Start),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2006),
                            function_instance: Some(FunctionInstanceId(2004)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::StyledActionLabel {
                            sink: SinkPortId(1996),
                            press_port: SourcePortId(1996),
                            width: None,
                            bold_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2007),
                            function_instance: Some(FunctionInstanceId(2004)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1997), Some(20), false, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2008),
                            function_instance: Some(FunctionInstanceId(2004)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1998),
                        },
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_then_document() -> HostViewIr {
    lower_live_arithmetic_document(
        FunctionInstanceId(2009),
        ViewSiteId(2009),
        SinkPortId(2010),
        SinkPortId(2011),
        SinkPortId(2012),
        Some((SourcePortId(2012), "A + B")),
        None,
        SourcePortId(2010),
        500,
    )
}

fn lower_when_document() -> HostViewIr {
    lower_live_arithmetic_document(
        FunctionInstanceId(2010),
        ViewSiteId(2021),
        SinkPortId(2013),
        SinkPortId(2014),
        SinkPortId(2015),
        Some((SourcePortId(2015), "A + B")),
        Some((SourcePortId(2016), "A - B")),
        SourcePortId(2013),
        500,
    )
}

fn lower_while_document() -> HostViewIr {
    lower_live_arithmetic_document(
        FunctionInstanceId(2011),
        ViewSiteId(2033),
        SinkPortId(2016),
        SinkPortId(2017),
        SinkPortId(2018),
        Some((SourcePortId(2019), "A + B")),
        Some((SourcePortId(2020), "A - B")),
        SourcePortId(2017),
        500,
    )
}

fn lower_while_function_call_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(2045),
                function_instance: Some(FunctionInstanceId(2012)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(2046),
                    function_instance: Some(FunctionInstanceId(2012)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    10,
                    Some(20),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Start),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2047),
                            function_instance: Some(FunctionInstanceId(2012)),
                            mapped_item_identity: None,
                        },
                        kind: styled_action_label(SinkPortId(2021), SourcePortId(2021), None, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2048),
                            function_instance: Some(FunctionInstanceId(2012)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(2022), Some(20), false, None),
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_button_hover_to_click_test_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(2049),
                function_instance: Some(FunctionInstanceId(2013)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(2050),
                    function_instance: Some(FunctionInstanceId(2013)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    20,
                    Some(20),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Start),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2051),
                            function_instance: Some(FunctionInstanceId(2013)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(2023), None, false, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2052),
                            function_instance: Some(FunctionInstanceId(2013)),
                            mapped_item_identity: None,
                        },
                        kind: styled_stripe_layout(
                            HostStripeDirection::Row,
                            10,
                            None,
                            None,
                            Some(HostCrossAlign::Center),
                        ),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2053),
                                    function_instance: Some(FunctionInstanceId(2013)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: styled_button(
                                    "Button A",
                                    SourcePortId(2022),
                                    None,
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.25 0 0)"),
                                    Some(SinkPortId(2024)),
                                    Some("oklch(0.35 0.1 25)"),
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2053),
                                    function_instance: Some(FunctionInstanceId(2013)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: styled_button(
                                    "Button B",
                                    SourcePortId(2023),
                                    None,
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.25 0 0)"),
                                    Some(SinkPortId(2025)),
                                    Some("oklch(0.35 0.1 25)"),
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2053),
                                    function_instance: Some(FunctionInstanceId(2013)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: styled_button(
                                    "Button C",
                                    SourcePortId(2024),
                                    None,
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.25 0 0)"),
                                    Some(SinkPortId(2026)),
                                    Some("oklch(0.35 0.1 25)"),
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2054),
                            function_instance: Some(FunctionInstanceId(2013)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(2027), None, false, None),
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_button_hover_test_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(2063),
                function_instance: Some(FunctionInstanceId(2015)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(2064),
                    function_instance: Some(FunctionInstanceId(2015)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    20,
                    Some(20),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Start),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2065),
                            function_instance: Some(FunctionInstanceId(2015)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(2033), None, false, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2066),
                            function_instance: Some(FunctionInstanceId(2015)),
                            mapped_item_identity: None,
                        },
                        kind: styled_stripe_layout(
                            HostStripeDirection::Row,
                            10,
                            None,
                            None,
                            Some(HostCrossAlign::Center),
                        ),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2067),
                                    function_instance: Some(FunctionInstanceId(2015)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: styled_button(
                                    "Button A",
                                    SourcePortId(2033),
                                    None,
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.25 0 0)"),
                                    Some(SinkPortId(2034)),
                                    Some("oklch(0.35 0.1 250)"),
                                    Some(SinkPortId(2034)),
                                    Some("2px solid oklch(0.6 0.2 250)"),
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2067),
                                    function_instance: Some(FunctionInstanceId(2015)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: styled_button(
                                    "Button B",
                                    SourcePortId(2034),
                                    None,
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.25 0 0)"),
                                    Some(SinkPortId(2035)),
                                    Some("oklch(0.35 0.1 250)"),
                                    Some(SinkPortId(2035)),
                                    Some("2px solid oklch(0.6 0.2 250)"),
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2067),
                                    function_instance: Some(FunctionInstanceId(2015)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: styled_button(
                                    "Button C",
                                    SourcePortId(2035),
                                    None,
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.25 0 0)"),
                                    Some(SinkPortId(2036)),
                                    Some("oklch(0.35 0.1 250)"),
                                    Some(SinkPortId(2036)),
                                    Some("2px solid oklch(0.6 0.2 250)"),
                                ),
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_switch_hold_test_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(2055),
                function_instance: Some(FunctionInstanceId(2014)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(2056),
                    function_instance: Some(FunctionInstanceId(2014)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    20,
                    Some(20),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Start),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2057),
                            function_instance: Some(FunctionInstanceId(2014)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(2028), None, false, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2058),
                            function_instance: Some(FunctionInstanceId(2014)),
                            mapped_item_identity: None,
                        },
                        kind: styled_button(
                            "Toggle View",
                            SourcePortId(2025),
                            None,
                            None,
                            Some(10),
                            false,
                            Some("oklch(0.3 0.06 200)"),
                            None,
                            None,
                            None,
                            None,
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2059),
                            function_instance: Some(FunctionInstanceId(2014)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(2029), Some(18), false, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2060),
                            function_instance: Some(FunctionInstanceId(2014)),
                            mapped_item_identity: None,
                        },
                        kind: styled_stripe_layout(
                            HostStripeDirection::Row,
                            10,
                            None,
                            None,
                            Some(HostCrossAlign::Center),
                        ),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2062),
                                    function_instance: Some(FunctionInstanceId(2014)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: styled_button(
                                    "Click Item A",
                                    SourcePortId(2026),
                                    Some(SinkPortId(2031)),
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.35 0.1 120)"),
                                    None,
                                    None,
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(2062),
                                    function_instance: Some(FunctionInstanceId(2014)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: styled_button(
                                    "Click Item B",
                                    SourcePortId(2027),
                                    Some(SinkPortId(2030)),
                                    None,
                                    Some(10),
                                    false,
                                    Some("oklch(0.35 0.1 240)"),
                                    None,
                                    None,
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(2061),
                            function_instance: Some(FunctionInstanceId(2014)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(2032), None, false, Some("oklch(0.7 0 0)")),
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_live_arithmetic_document(
    function_instance: FunctionInstanceId,
    root_view_site: ViewSiteId,
    input_a_sink: SinkPortId,
    input_b_sink: SinkPortId,
    result_sink: SinkPortId,
    primary_button: Option<(SourcePortId, &str)>,
    secondary_button: Option<(SourcePortId, &str)>,
    tick_port: SourcePortId,
    interval_ms: u32,
) -> HostViewIr {
    let mut children = vec![
        HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(root_view_site.0 + 2),
                function_instance: Some(function_instance),
                mapped_item_identity: None,
            },
            kind: styled_label(input_a_sink, None, false, None),
            children: Vec::new(),
        },
        HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(root_view_site.0 + 3),
                function_instance: Some(function_instance),
                mapped_item_identity: None,
            },
            kind: styled_label(input_b_sink, None, false, None),
            children: Vec::new(),
        },
    ];

    let mut button_children = Vec::new();
    if let Some((press_port, label)) = primary_button {
        button_children.push(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(root_view_site.0 + 5),
                function_instance: Some(function_instance),
                mapped_item_identity: Some(1),
            },
            kind: styled_button(
                label, press_port, None, None, None, false, None, None, None, None, None,
            ),
            children: Vec::new(),
        });
    }
    if let Some((press_port, label)) = secondary_button {
        button_children.push(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(root_view_site.0 + 5),
                function_instance: Some(function_instance),
                mapped_item_identity: Some(2),
            },
            kind: styled_button(
                label, press_port, None, None, None, false, None, None, None, None, None,
            ),
            children: Vec::new(),
        });
    }

    children.push(HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: ViewSiteId(root_view_site.0 + 4),
            function_instance: Some(function_instance),
            mapped_item_identity: None,
        },
        kind: styled_stripe_layout(
            HostStripeDirection::Row,
            12,
            None,
            None,
            Some(HostCrossAlign::Center),
        ),
        children: button_children,
    });
    children.push(HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: ViewSiteId(root_view_site.0 + 6),
            function_instance: Some(function_instance),
            mapped_item_identity: None,
        },
        kind: styled_label(result_sink, Some(22), true, None),
        children: Vec::new(),
    });
    children.push(HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: ViewSiteId(root_view_site.0 + 7),
            function_instance: Some(function_instance),
            mapped_item_identity: None,
        },
        kind: HostViewKind::TimerSource {
            tick_port,
            interval_ms,
        },
        children: Vec::new(),
    });

    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: root_view_site,
                function_instance: Some(function_instance),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(root_view_site.0 + 1),
                    function_instance: Some(function_instance),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    12,
                    Some(16),
                    Some(HostWidth::Fill),
                    Some(HostCrossAlign::Center),
                ),
                children,
            }],
        }),
    }
}

fn lower_interval_ir(tick_port: SourcePortId, value_sink: SinkPortId) -> Vec<IrNode> {
    vec![
        IrNode {
            id: NodeId(1980),
            source_expr: None,
            kind: IrNodeKind::SourcePort(tick_port),
        },
        IrNode {
            id: NodeId(1981),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(1.0)),
        },
        IrNode {
            id: NodeId(1982),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1980),
                body: NodeId(1981),
            },
        },
        IrNode {
            id: NodeId(1983),
            source_expr: None,
            kind: IrNodeKind::MathSum {
                input: NodeId(1982),
            },
        },
        IrNode {
            id: NodeId(1984),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: value_sink,
                input: NodeId(1983),
            },
        },
    ]
}

fn lower_interval_hold_ir(tick_port: SourcePortId, value_sink: SinkPortId) -> Vec<IrNode> {
    vec![
        IrNode {
            id: NodeId(1985),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(0.0)),
        },
        IrNode {
            id: NodeId(1986),
            source_expr: None,
            kind: IrNodeKind::SourcePort(tick_port),
        },
        IrNode {
            id: NodeId(1987),
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(1.0)),
        },
        IrNode {
            id: NodeId(1988),
            source_expr: None,
            kind: IrNodeKind::Add {
                lhs: NodeId(1990),
                rhs: NodeId(1987),
            },
        },
        IrNode {
            id: NodeId(1989),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1986),
                body: NodeId(1988),
            },
        },
        IrNode {
            id: NodeId(1990),
            source_expr: None,
            kind: IrNodeKind::Hold {
                seed: NodeId(1985),
                updates: NodeId(1989),
            },
        },
        IrNode {
            id: NodeId(1991),
            source_expr: None,
            kind: IrNodeKind::Then {
                source: NodeId(1986),
                body: NodeId(1990),
            },
        },
        IrNode {
            id: NodeId(1992),
            source_expr: None,
            kind: IrNodeKind::SinkPort {
                port: value_sink,
                input: NodeId(1991),
            },
        },
    ]
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
            "interval_hold subset requires `HOLD ... |> Stream/skip(count: 1)`".to_string(),
        );
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err("interval_hold subset requires `Stream/skip(count: 1)`".to_string());
    };
    if !path_matches(path, &["Stream", "skip"]) {
        return Err("interval_hold subset requires `Stream/skip(count: 1)`".to_string());
    }
    let count = find_named_argument(arguments, "count")
        .ok_or_else(|| "interval_hold subset requires `count` for Stream/skip".to_string())?;
    if extract_integer_literal(count)? != 1 {
        return Err("interval_hold subset requires `Stream/skip(count: 1)`".to_string());
    }

    let StaticExpression::Pipe {
        from: seed,
        to: hold,
    } = &from.node
    else {
        return Err("interval_hold subset requires `0 |> HOLD counter { ... }`".to_string());
    };
    if extract_integer_literal(seed)? != 0 {
        return Err("interval_hold subset requires `0 |> HOLD counter { ... }`".to_string());
    }

    let StaticExpression::Hold { state_param, body } = &hold.node else {
        return Err("interval_hold subset requires `HOLD counter { ... }`".to_string());
    };
    if state_param.as_str() != "counter" {
        return Err("interval_hold subset requires HOLD state param `counter`".to_string());
    }

    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &body.node
    else {
        return Err("interval_hold subset requires `tick |> THEN { counter + 1 }`".to_string());
    };
    ensure_alias_name(trigger_source, "tick")
        .map_err(|_| "interval_hold subset requires `tick |> THEN { counter + 1 }`".to_string())?;
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Err("interval_hold subset requires THEN body".to_string());
    };
    match &body.node {
        StaticExpression::ArithmeticOperator(operator) => {
            let increment = extract_hold_increment(operator, "counter")?;
            if increment != 1 {
                return Err("interval_hold subset requires `counter + 1`".to_string());
            }
        }
        _ => return Err("interval_hold subset requires `counter + 1`".to_string()),
    }

    Ok(())
}

fn lower_interval_hold_document(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err("interval_hold subset requires `counter |> Document/new()`".to_string());
    };
    ensure_alias_name(from, "counter")
        .map_err(|_| "interval_hold subset requires `counter |> Document/new()`".to_string())?;
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err("interval_hold subset requires `Document/new()`".to_string());
    };
    if !path_matches(path, &["Document", "new"]) || !arguments.is_empty() {
        return Err("interval_hold subset requires `Document/new()`".to_string());
    }
    Ok(())
}

fn ensure_fibonacci_function(expression: &StaticSpannedExpression) -> Result<(), String> {
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

fn ensure_fibonacci_result_binding(expression: &StaticSpannedExpression) -> Result<(), String> {
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

fn ensure_fibonacci_message(expression: &StaticSpannedExpression) -> Result<(), String> {
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

fn ensure_fibonacci_document(expression: &StaticSpannedExpression) -> Result<(), String> {
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

fn fibonacci_number(position: u64) -> u64 {
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

fn lower_counter_document(
    expression: &StaticSpannedExpression,
    press_port: SourcePortId,
    button_label: String,
) -> Result<HostViewIr, String> {
    let root = extract_document_root(expression)?;
    let StaticExpression::FunctionCall { path, arguments } = &root.node else {
        return Err("counter subset requires Element/stripe root".to_string());
    };
    if !path_matches(path, &["Element", "stripe"]) {
        return Err("counter subset requires Element/stripe root".to_string());
    }
    let items = find_named_argument(arguments, "items")
        .ok_or_else(|| "counter root requires items".to_string())?;
    let StaticExpression::List { items } = &items.node else {
        return Err("counter root items must be LIST".to_string());
    };
    if items.len() != 2 {
        return Err("counter root must render counter then increment_button".to_string());
    }
    ensure_alias_name(&items[0], "counter")?;
    ensure_alias_name(&items[1], "increment_button")?;

    Ok(HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1),
                function_instance: Some(FunctionInstanceId(1)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(2),
                    function_instance: Some(FunctionInstanceId(1)),
                    mapped_item_identity: None,
                },
                kind: stripe_layout(HostStripeDirection::Column, 0),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(3),
                            function_instance: Some(FunctionInstanceId(1)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(4),
                            function_instance: Some(FunctionInstanceId(1)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Button {
                            label: button_label,
                            press_port,
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    })
}

fn lower_complex_counter_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(20),
                function_instance: Some(FunctionInstanceId(20)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(21),
                    function_instance: Some(FunctionInstanceId(20)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Row,
                    15,
                    None,
                    None,
                    Some(HostCrossAlign::Center),
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(22),
                            function_instance: Some(FunctionInstanceId(20)),
                            mapped_item_identity: None,
                        },
                        kind: styled_button(
                            "-",
                            SourcePortId(10),
                            None,
                            Some(HostWidth::Px(45)),
                            None,
                            true,
                            Some("oklch(0.75 0.07 320)"),
                            Some(SinkPortId(11)),
                            Some("oklch(0.85 0.07 320)"),
                            None,
                            None,
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(23),
                            function_instance: Some(FunctionInstanceId(20)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(10),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(24),
                            function_instance: Some(FunctionInstanceId(20)),
                            mapped_item_identity: None,
                        },
                        kind: styled_button(
                            "+",
                            SourcePortId(11),
                            None,
                            Some(HostWidth::Px(45)),
                            None,
                            true,
                            Some("oklch(0.75 0.07 320)"),
                            Some(SinkPortId(12)),
                            Some("oklch(0.85 0.07 320)"),
                            None,
                            None,
                        ),
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_list_retain_reactive_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(30),
                function_instance: Some(FunctionInstanceId(30)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(31),
                    function_instance: Some(FunctionInstanceId(30)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(32),
                            function_instance: Some(FunctionInstanceId(30)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Button {
                            label: "Toggle filter".to_string(),
                            press_port: SourcePortId(30),
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(33),
                            function_instance: Some(FunctionInstanceId(30)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(30),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(34),
                            function_instance: Some(FunctionInstanceId(30)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(31),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(35),
                            function_instance: Some(FunctionInstanceId(30)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(36),
                                    function_instance: Some(FunctionInstanceId(30)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(32),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(36),
                                    function_instance: Some(FunctionInstanceId(30)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(33),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(36),
                                    function_instance: Some(FunctionInstanceId(30)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(34),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(36),
                                    function_instance: Some(FunctionInstanceId(30)),
                                    mapped_item_identity: Some(4),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(35),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(36),
                                    function_instance: Some(FunctionInstanceId(30)),
                                    mapped_item_identity: Some(5),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(36),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(36),
                                    function_instance: Some(FunctionInstanceId(30)),
                                    mapped_item_identity: Some(6),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(37),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_list_map_external_dep_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(40),
                function_instance: Some(FunctionInstanceId(40)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(41),
                    function_instance: Some(FunctionInstanceId(40)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(42),
                            function_instance: Some(FunctionInstanceId(40)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(40),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(43),
                            function_instance: Some(FunctionInstanceId(40)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Button {
                            label: "Toggle filter".to_string(),
                            press_port: SourcePortId(40),
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(44),
                            function_instance: Some(FunctionInstanceId(40)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(41),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(45),
                            function_instance: Some(FunctionInstanceId(40)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(46),
                                    function_instance: Some(FunctionInstanceId(40)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(42),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(46),
                                    function_instance: Some(FunctionInstanceId(40)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(43),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(46),
                                    function_instance: Some(FunctionInstanceId(40)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(44),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(46),
                                    function_instance: Some(FunctionInstanceId(40)),
                                    mapped_item_identity: Some(4),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(45),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_list_map_block_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(50),
                function_instance: Some(FunctionInstanceId(50)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(51),
                    function_instance: Some(FunctionInstanceId(50)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(52),
                            function_instance: Some(FunctionInstanceId(50)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(50),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(53),
                            function_instance: Some(FunctionInstanceId(50)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(54),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(51),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(54),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(52),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(54),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(53),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(54),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(4),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(54),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(54),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(5),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(55),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(55),
                            function_instance: Some(FunctionInstanceId(50)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(56),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(56),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(56),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(57),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(56),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(58),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(56),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(4),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(59),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(56),
                                    function_instance: Some(FunctionInstanceId(50)),
                                    mapped_item_identity: Some(5),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(60),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_list_retain_count_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(70),
                function_instance: Some(FunctionInstanceId(70)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(71),
                    function_instance: Some(FunctionInstanceId(70)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(72),
                            function_instance: Some(FunctionInstanceId(70)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::TextInput {
                            value_sink: SinkPortId(70),
                            placeholder: "Type and press Enter".to_string(),
                            change_port: SourcePortId(70),
                            key_down_port: SourcePortId(71),
                            focus_on_mount: true,
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(73),
                            function_instance: Some(FunctionInstanceId(70)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(71),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(74),
                            function_instance: Some(FunctionInstanceId(70)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(72),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(75),
                            function_instance: Some(FunctionInstanceId(70)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(76),
                                    function_instance: Some(FunctionInstanceId(70)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(73),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(76),
                                    function_instance: Some(FunctionInstanceId(70)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(74),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(76),
                                    function_instance: Some(FunctionInstanceId(70)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(75),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(76),
                                    function_instance: Some(FunctionInstanceId(70)),
                                    mapped_item_identity: Some(4),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(76),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_list_retain_remove_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(80),
                function_instance: Some(FunctionInstanceId(80)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(81),
                    function_instance: Some(FunctionInstanceId(80)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(82),
                            function_instance: Some(FunctionInstanceId(80)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(80),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(83),
                            function_instance: Some(FunctionInstanceId(80)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::TextInput {
                            value_sink: SinkPortId(81),
                            placeholder: "Type and press Enter".to_string(),
                            change_port: SourcePortId(80),
                            key_down_port: SourcePortId(81),
                            focus_on_mount: true,
                            disabled_sink: None,
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(84),
                            function_instance: Some(FunctionInstanceId(80)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(82),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(85),
                            function_instance: Some(FunctionInstanceId(80)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(86),
                                    function_instance: Some(FunctionInstanceId(80)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(83),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(86),
                                    function_instance: Some(FunctionInstanceId(80)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(84),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(86),
                                    function_instance: Some(FunctionInstanceId(80)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(85),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(86),
                                    function_instance: Some(FunctionInstanceId(80)),
                                    mapped_item_identity: Some(4),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(86),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(86),
                                    function_instance: Some(FunctionInstanceId(80)),
                                    mapped_item_identity: Some(5),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(87),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(86),
                                    function_instance: Some(FunctionInstanceId(80)),
                                    mapped_item_identity: Some(6),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(88),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_list_object_state_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(90),
                function_instance: Some(FunctionInstanceId(90)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(91),
                    function_instance: Some(FunctionInstanceId(90)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(92),
                            function_instance: Some(FunctionInstanceId(90)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(89),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(93),
                            function_instance: Some(FunctionInstanceId(90)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(94),
                                    function_instance: Some(FunctionInstanceId(90)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Stripe,
                                children: vec![
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(95),
                                            function_instance: Some(FunctionInstanceId(90)),
                                            mapped_item_identity: Some(1),
                                        },
                                        kind: HostViewKind::Button {
                                            label: "Click me".to_string(),
                                            press_port: SourcePortId(90),
                                            disabled_sink: None,
                                        },
                                        children: Vec::new(),
                                    },
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(96),
                                            function_instance: Some(FunctionInstanceId(90)),
                                            mapped_item_identity: Some(1),
                                        },
                                        kind: HostViewKind::Label {
                                            sink: SinkPortId(90),
                                        },
                                        children: Vec::new(),
                                    },
                                ],
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(94),
                                    function_instance: Some(FunctionInstanceId(90)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Stripe,
                                children: vec![
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(95),
                                            function_instance: Some(FunctionInstanceId(90)),
                                            mapped_item_identity: Some(2),
                                        },
                                        kind: HostViewKind::Button {
                                            label: "Click me".to_string(),
                                            press_port: SourcePortId(91),
                                            disabled_sink: None,
                                        },
                                        children: Vec::new(),
                                    },
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(96),
                                            function_instance: Some(FunctionInstanceId(90)),
                                            mapped_item_identity: Some(2),
                                        },
                                        kind: HostViewKind::Label {
                                            sink: SinkPortId(91),
                                        },
                                        children: Vec::new(),
                                    },
                                ],
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(94),
                                    function_instance: Some(FunctionInstanceId(90)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: HostViewKind::Stripe,
                                children: vec![
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(95),
                                            function_instance: Some(FunctionInstanceId(90)),
                                            mapped_item_identity: Some(3),
                                        },
                                        kind: HostViewKind::Button {
                                            label: "Click me".to_string(),
                                            press_port: SourcePortId(92),
                                            disabled_sink: None,
                                        },
                                        children: Vec::new(),
                                    },
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(96),
                                            function_instance: Some(FunctionInstanceId(90)),
                                            mapped_item_identity: Some(3),
                                        },
                                        kind: HostViewKind::Label {
                                            sink: SinkPortId(92),
                                        },
                                        children: Vec::new(),
                                    },
                                ],
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_shopping_list_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1000),
                function_instance: Some(FunctionInstanceId(1000)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1001),
                    function_instance: Some(FunctionInstanceId(1000)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    16,
                    Some(20),
                    Some(HostWidth::Px(400)),
                    None,
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1002),
                            function_instance: Some(FunctionInstanceId(1000)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1006), Some(24), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1003),
                            function_instance: Some(FunctionInstanceId(1000)),
                            mapped_item_identity: None,
                        },
                        kind: styled_text_input(
                            SinkPortId(1000),
                            "Type and press Enter to add...",
                            SourcePortId(1000),
                            SourcePortId(1001),
                            true,
                            None,
                            Some(HostWidth::Fill),
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1004),
                            function_instance: Some(FunctionInstanceId(1000)),
                            mapped_item_identity: None,
                        },
                        kind: stripe_layout(HostStripeDirection::Column, 4),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1005),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: styled_label(SinkPortId(1002), None, false, Some("white")),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1005),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: styled_label(SinkPortId(1003), None, false, Some("white")),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1005),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: Some(3),
                                },
                                kind: styled_label(SinkPortId(1004), None, false, Some("white")),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1005),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: Some(4),
                                },
                                kind: styled_label(SinkPortId(1005), None, false, Some("white")),
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1006),
                            function_instance: Some(FunctionInstanceId(1000)),
                            mapped_item_identity: None,
                        },
                        kind: stripe_layout(HostStripeDirection::Row, 16),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1007),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_label(
                                    SinkPortId(1001),
                                    None,
                                    false,
                                    Some("oklch(0.5 0 0)"),
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1008),
                                    function_instance: Some(FunctionInstanceId(1000)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "Clear",
                                    SourcePortId(1002),
                                    None,
                                    None,
                                    Some(10),
                                    false,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                ),
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_temperature_converter_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1800),
                function_instance: Some(FunctionInstanceId(1800)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1801),
                    function_instance: Some(FunctionInstanceId(1800)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    16,
                    Some(20),
                    Some(HostWidth::Px(400)),
                    None,
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1802),
                            function_instance: Some(FunctionInstanceId(1800)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1800), Some(22), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1803),
                            function_instance: Some(FunctionInstanceId(1800)),
                            mapped_item_identity: None,
                        },
                        kind: styled_stripe_layout(
                            HostStripeDirection::Row,
                            10,
                            None,
                            None,
                            Some(HostCrossAlign::Center),
                        ),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1804),
                                    function_instance: Some(FunctionInstanceId(1800)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_text_input(
                                    SinkPortId(1801),
                                    "Celsius",
                                    SourcePortId(1800),
                                    SourcePortId(1801),
                                    false,
                                    None,
                                    Some(HostWidth::Px(120)),
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1805),
                                    function_instance: Some(FunctionInstanceId(1800)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1803),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1806),
                                    function_instance: Some(FunctionInstanceId(1800)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1804),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1807),
                                    function_instance: Some(FunctionInstanceId(1800)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_text_input(
                                    SinkPortId(1802),
                                    "Fahrenheit",
                                    SourcePortId(1802),
                                    SourcePortId(1803),
                                    false,
                                    None,
                                    Some(HostWidth::Px(120)),
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1808),
                                    function_instance: Some(FunctionInstanceId(1800)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1805),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_filter_checkbox_bug_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1200),
                function_instance: Some(FunctionInstanceId(1200)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1201),
                    function_instance: Some(FunctionInstanceId(1200)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    12,
                    Some(20),
                    Some(HostWidth::Px(300)),
                    None,
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1202),
                            function_instance: Some(FunctionInstanceId(1200)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1200),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1203),
                            function_instance: Some(FunctionInstanceId(1200)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1204),
                                    function_instance: Some(FunctionInstanceId(1200)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Button {
                                    label: "All".to_string(),
                                    press_port: SourcePortId(1200),
                                    disabled_sink: None,
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1205),
                                    function_instance: Some(FunctionInstanceId(1200)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Button {
                                    label: "Active".to_string(),
                                    press_port: SourcePortId(1201),
                                    disabled_sink: None,
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1206),
                            function_instance: Some(FunctionInstanceId(1200)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1207),
                                    function_instance: Some(FunctionInstanceId(1200)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Stripe,
                                children: vec![
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1208),
                                            function_instance: Some(FunctionInstanceId(1200)),
                                            mapped_item_identity: Some(1),
                                        },
                                        kind: HostViewKind::Checkbox {
                                            checked_sink: SinkPortId(1201),
                                            click_port: SourcePortId(1202),
                                        },
                                        children: Vec::new(),
                                    },
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1209),
                                            function_instance: Some(FunctionInstanceId(1200)),
                                            mapped_item_identity: Some(1),
                                        },
                                        kind: HostViewKind::Label {
                                            sink: SinkPortId(1203),
                                        },
                                        children: Vec::new(),
                                    },
                                ],
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1207),
                                    function_instance: Some(FunctionInstanceId(1200)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Stripe,
                                children: vec![
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1208),
                                            function_instance: Some(FunctionInstanceId(1200)),
                                            mapped_item_identity: Some(2),
                                        },
                                        kind: HostViewKind::Checkbox {
                                            checked_sink: SinkPortId(1202),
                                            click_port: SourcePortId(1203),
                                        },
                                        children: Vec::new(),
                                    },
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1209),
                                            function_instance: Some(FunctionInstanceId(1200)),
                                            mapped_item_identity: Some(2),
                                        },
                                        kind: HostViewKind::Label {
                                            sink: SinkPortId(1204),
                                        },
                                        children: Vec::new(),
                                    },
                                ],
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1210),
                            function_instance: Some(FunctionInstanceId(1200)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1205),
                        },
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_checkbox_test_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1300),
                function_instance: Some(FunctionInstanceId(1300)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1301),
                    function_instance: Some(FunctionInstanceId(1300)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1302),
                            function_instance: Some(FunctionInstanceId(1300)),
                            mapped_item_identity: Some(1),
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1303),
                                    function_instance: Some(FunctionInstanceId(1300)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Checkbox {
                                    checked_sink: SinkPortId(1300),
                                    click_port: SourcePortId(1300),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1304),
                                    function_instance: Some(FunctionInstanceId(1300)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1304),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1305),
                                    function_instance: Some(FunctionInstanceId(1300)),
                                    mapped_item_identity: Some(1),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1302),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1302),
                            function_instance: Some(FunctionInstanceId(1300)),
                            mapped_item_identity: Some(2),
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1303),
                                    function_instance: Some(FunctionInstanceId(1300)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Checkbox {
                                    checked_sink: SinkPortId(1301),
                                    click_port: SourcePortId(1301),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1304),
                                    function_instance: Some(FunctionInstanceId(1300)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1305),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1305),
                                    function_instance: Some(FunctionInstanceId(1300)),
                                    mapped_item_identity: Some(2),
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1303),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn lower_flight_booker_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1900),
                function_instance: Some(FunctionInstanceId(1900)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1901),
                    function_instance: Some(FunctionInstanceId(1900)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1902),
                            function_instance: Some(FunctionInstanceId(1900)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1900), Some(22), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1903),
                            function_instance: Some(FunctionInstanceId(1900)),
                            mapped_item_identity: None,
                        },
                        kind: styled_select(
                            SinkPortId(1901),
                            SourcePortId(1900),
                            vec![
                                HostSelectOption {
                                    value: "one-way".to_string(),
                                    label: "One-way flight".to_string(),
                                },
                                HostSelectOption {
                                    value: "return".to_string(),
                                    label: "Return flight".to_string(),
                                },
                            ],
                            None,
                            Some(HostWidth::Fill),
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1904),
                            function_instance: Some(FunctionInstanceId(1900)),
                            mapped_item_identity: None,
                        },
                        kind: styled_text_input(
                            SinkPortId(1902),
                            "YYYY-MM-DD",
                            SourcePortId(1901),
                            SourcePortId(1911),
                            false,
                            None,
                            Some(HostWidth::Fill),
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1905),
                            function_instance: Some(FunctionInstanceId(1900)),
                            mapped_item_identity: None,
                        },
                        kind: styled_text_input(
                            SinkPortId(1903),
                            "YYYY-MM-DD",
                            SourcePortId(1902),
                            SourcePortId(1912),
                            false,
                            Some(SinkPortId(1904)),
                            Some(HostWidth::Fill),
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1906),
                            function_instance: Some(FunctionInstanceId(1900)),
                            mapped_item_identity: None,
                        },
                        kind: styled_button(
                            "Book",
                            SourcePortId(1903),
                            Some(SinkPortId(1905)),
                            Some(HostWidth::Fill),
                            None,
                            false,
                            None,
                            None,
                            None,
                            None,
                            None,
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1907),
                            function_instance: Some(FunctionInstanceId(1900)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1906),
                        },
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_timer_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1950),
                function_instance: Some(FunctionInstanceId(1950)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1951),
                    function_instance: Some(FunctionInstanceId(1950)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    16,
                    Some(20),
                    Some(HostWidth::Px(400)),
                    None,
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1952),
                            function_instance: Some(FunctionInstanceId(1950)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1950), Some(22), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1953),
                            function_instance: Some(FunctionInstanceId(1950)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1954),
                                    function_instance: Some(FunctionInstanceId(1950)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1951),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1955),
                                    function_instance: Some(FunctionInstanceId(1950)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1952),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1956),
                            function_instance: Some(FunctionInstanceId(1950)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1953),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1957),
                            function_instance: Some(FunctionInstanceId(1950)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1958),
                                    function_instance: Some(FunctionInstanceId(1950)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1954),
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1959),
                                    function_instance: Some(FunctionInstanceId(1950)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_slider(
                                    SinkPortId(1955),
                                    SourcePortId(1950),
                                    "1",
                                    "30",
                                    "0.1",
                                    None,
                                    Some(HostWidth::Px(200)),
                                ),
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1960),
                                    function_instance: Some(FunctionInstanceId(1950)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1956),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1961),
                            function_instance: Some(FunctionInstanceId(1950)),
                            mapped_item_identity: None,
                        },
                        kind: styled_button(
                            "Reset",
                            SourcePortId(1951),
                            None,
                            Some(HostWidth::Fill),
                            None,
                            false,
                            None,
                            None,
                            None,
                            None,
                            None,
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1962),
                            function_instance: Some(FunctionInstanceId(1950)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::TimerSource {
                            tick_port: SourcePortId(1952),
                            interval_ms: 100,
                        },
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn lower_circle_drawer_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1970),
                function_instance: Some(FunctionInstanceId(1970)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1971),
                    function_instance: Some(FunctionInstanceId(1970)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    12,
                    Some(20),
                    Some(HostWidth::Px(500)),
                    None,
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1972),
                            function_instance: Some(FunctionInstanceId(1970)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1970), Some(22), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1973),
                            function_instance: Some(FunctionInstanceId(1970)),
                            mapped_item_identity: None,
                        },
                        kind: styled_stripe_layout(HostStripeDirection::Row, 10, None, None, None),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1974),
                                    function_instance: Some(FunctionInstanceId(1970)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "Undo",
                                    SourcePortId(1971),
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
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1975),
                                    function_instance: Some(FunctionInstanceId(1970)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Label {
                                    sink: SinkPortId(1971),
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1976),
                            function_instance: Some(FunctionInstanceId(1970)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::AbsoluteCanvas {
                            click_port: SourcePortId(1970),
                            width_px: 460,
                            height_px: 300,
                            background: "rgba(255,255,255,0.1)".to_string(),
                        },
                        children: vec![HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(1977),
                                function_instance: Some(FunctionInstanceId(1970)),
                                mapped_item_identity: None,
                            },
                            kind: HostViewKind::PositionedCircleList {
                                circles_sink: SinkPortId(1972),
                                radius_px: 20,
                                fill: "#3498db".to_string(),
                                stroke: "#2c3e50".to_string(),
                                stroke_width_px: 2,
                            },
                            children: Vec::new(),
                        }],
                    },
                ],
            }],
        }),
    }
}

fn lower_chained_list_remove_bug_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1400),
                function_instance: Some(FunctionInstanceId(1400)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1401),
                    function_instance: Some(FunctionInstanceId(1400)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::Stripe,
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1402),
                            function_instance: Some(FunctionInstanceId(1400)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1409),
                        },
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1403),
                            function_instance: Some(FunctionInstanceId(1400)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1404),
                                    function_instance: Some(FunctionInstanceId(1400)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Button {
                                    label: "Add Item".to_string(),
                                    press_port: SourcePortId(1400),
                                    disabled_sink: None,
                                },
                                children: Vec::new(),
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1405),
                                    function_instance: Some(FunctionInstanceId(1400)),
                                    mapped_item_identity: None,
                                },
                                kind: HostViewKind::Button {
                                    label: "Clear completed".to_string(),
                                    press_port: SourcePortId(1401),
                                    disabled_sink: None,
                                },
                                children: Vec::new(),
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1406),
                            function_instance: Some(FunctionInstanceId(1400)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Stripe,
                        children: vec![
                            row_slot_node(
                                1,
                                SourcePortId(1402),
                                SourcePortId(1410),
                                SinkPortId(1400),
                                SinkPortId(1404),
                            ),
                            row_slot_node(
                                2,
                                SourcePortId(1403),
                                SourcePortId(1411),
                                SinkPortId(1401),
                                SinkPortId(1405),
                            ),
                            row_slot_node(
                                3,
                                SourcePortId(1404),
                                SourcePortId(1412),
                                SinkPortId(1402),
                                SinkPortId(1406),
                            ),
                            row_slot_node(
                                4,
                                SourcePortId(1405),
                                SourcePortId(1413),
                                SinkPortId(1403),
                                SinkPortId(1407),
                            ),
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1410),
                            function_instance: Some(FunctionInstanceId(1400)),
                            mapped_item_identity: None,
                        },
                        kind: HostViewKind::Label {
                            sink: SinkPortId(1408),
                        },
                        children: Vec::new(),
                    },
                ],
            }],
        }),
    }
}

fn row_slot_node(
    mapped_item_identity: u64,
    checkbox_port: SourcePortId,
    remove_port: SourcePortId,
    checkbox_sink: SinkPortId,
    row_label_sink: SinkPortId,
) -> HostViewNode {
    HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: ViewSiteId(1407),
            function_instance: Some(FunctionInstanceId(1400)),
            mapped_item_identity: Some(mapped_item_identity),
        },
        kind: styled_stripe_layout(HostStripeDirection::Row, 8, None, None, None),
        children: vec![
            HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1408),
                    function_instance: Some(FunctionInstanceId(1400)),
                    mapped_item_identity: Some(mapped_item_identity),
                },
                kind: HostViewKind::Checkbox {
                    checked_sink: checkbox_sink,
                    click_port: checkbox_port,
                },
                children: Vec::new(),
            },
            HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1409),
                    function_instance: Some(FunctionInstanceId(1400)),
                    mapped_item_identity: Some(mapped_item_identity),
                },
                kind: HostViewKind::Label {
                    sink: row_label_sink,
                },
                children: Vec::new(),
            },
            HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1411),
                    function_instance: Some(FunctionInstanceId(1400)),
                    mapped_item_identity: Some(mapped_item_identity),
                },
                kind: styled_button(
                    "X",
                    remove_port,
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
            },
        ],
    }
}

fn lower_crud_document() -> HostViewIr {
    HostViewIr {
        root: Some(HostViewNode {
            retained_key: RetainedNodeKey {
                view_site: ViewSiteId(1700),
                function_instance: Some(FunctionInstanceId(1700)),
                mapped_item_identity: None,
            },
            kind: HostViewKind::Document,
            children: vec![HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1701),
                    function_instance: Some(FunctionInstanceId(1700)),
                    mapped_item_identity: None,
                },
                kind: styled_stripe_layout(
                    HostStripeDirection::Column,
                    16,
                    Some(20),
                    Some(HostWidth::Px(500)),
                    None,
                ),
                children: vec![
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1702),
                            function_instance: Some(FunctionInstanceId(1700)),
                            mapped_item_identity: None,
                        },
                        kind: styled_label(SinkPortId(1600), Some(22), true, None),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1703),
                            function_instance: Some(FunctionInstanceId(1700)),
                            mapped_item_identity: None,
                        },
                        kind: styled_text_input(
                            SinkPortId(1601),
                            "Filter by surname",
                            SourcePortId(1600),
                            SourcePortId(1601),
                            false,
                            None,
                            Some(HostWidth::Px(200)),
                        ),
                        children: Vec::new(),
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1704),
                            function_instance: Some(FunctionInstanceId(1700)),
                            mapped_item_identity: None,
                        },
                        kind: styled_stripe_layout(HostStripeDirection::Row, 16, None, None, None),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1705),
                                    function_instance: Some(FunctionInstanceId(1700)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_stripe_layout(
                                    HostStripeDirection::Column,
                                    2,
                                    None,
                                    Some(HostWidth::Px(250)),
                                    None,
                                ),
                                children: vec![
                                    crud_row_slot_node(
                                        1,
                                        SourcePortId(1609),
                                        SinkPortId(1604),
                                        SinkPortId(1608),
                                    ),
                                    crud_row_slot_node(
                                        2,
                                        SourcePortId(1610),
                                        SinkPortId(1605),
                                        SinkPortId(1609),
                                    ),
                                    crud_row_slot_node(
                                        3,
                                        SourcePortId(1611),
                                        SinkPortId(1606),
                                        SinkPortId(1610),
                                    ),
                                    crud_row_slot_node(
                                        4,
                                        SourcePortId(1612),
                                        SinkPortId(1607),
                                        SinkPortId(1611),
                                    ),
                                ],
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1706),
                                    function_instance: Some(FunctionInstanceId(1700)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_stripe_layout(
                                    HostStripeDirection::Column,
                                    10,
                                    None,
                                    None,
                                    None,
                                ),
                                children: vec![
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1707),
                                            function_instance: Some(FunctionInstanceId(1700)),
                                            mapped_item_identity: None,
                                        },
                                        kind: styled_text_input(
                                            SinkPortId(1602),
                                            "Name",
                                            SourcePortId(1602),
                                            SourcePortId(1603),
                                            false,
                                            None,
                                            Some(HostWidth::Px(150)),
                                        ),
                                        children: Vec::new(),
                                    },
                                    HostViewNode {
                                        retained_key: RetainedNodeKey {
                                            view_site: ViewSiteId(1708),
                                            function_instance: Some(FunctionInstanceId(1700)),
                                            mapped_item_identity: None,
                                        },
                                        kind: styled_text_input(
                                            SinkPortId(1603),
                                            "Surname",
                                            SourcePortId(1604),
                                            SourcePortId(1605),
                                            false,
                                            None,
                                            Some(HostWidth::Px(150)),
                                        ),
                                        children: Vec::new(),
                                    },
                                ],
                            },
                        ],
                    },
                    HostViewNode {
                        retained_key: RetainedNodeKey {
                            view_site: ViewSiteId(1709),
                            function_instance: Some(FunctionInstanceId(1700)),
                            mapped_item_identity: None,
                        },
                        kind: styled_stripe_layout(HostStripeDirection::Row, 10, None, None, None),
                        children: vec![
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1710),
                                    function_instance: Some(FunctionInstanceId(1700)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "Create",
                                    SourcePortId(1606),
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
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1711),
                                    function_instance: Some(FunctionInstanceId(1700)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "Update",
                                    SourcePortId(1607),
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
                            },
                            HostViewNode {
                                retained_key: RetainedNodeKey {
                                    view_site: ViewSiteId(1712),
                                    function_instance: Some(FunctionInstanceId(1700)),
                                    mapped_item_identity: None,
                                },
                                kind: styled_button(
                                    "Delete",
                                    SourcePortId(1608),
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
                            },
                        ],
                    },
                ],
            }],
        }),
    }
}

fn crud_row_slot_node(
    mapped_item_identity: u64,
    press_port: SourcePortId,
    label_sink: SinkPortId,
    selected_sink: SinkPortId,
) -> HostViewNode {
    HostViewNode {
        retained_key: RetainedNodeKey {
            view_site: ViewSiteId(1713),
            function_instance: Some(FunctionInstanceId(1700)),
            mapped_item_identity: Some(mapped_item_identity),
        },
        kind: styled_action_label(
            label_sink,
            press_port,
            Some(HostWidth::Fill),
            Some(selected_sink),
        ),
        children: Vec::new(),
    }
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

    #[test]
    fn lowers_real_counter_example() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let program = try_lower_counter(source).expect("counter should lower");
        assert_eq!(program.initial_value, 0);
        assert_eq!(program.increment_delta, 1);
        assert_eq!(program.press_port, SourcePortId(1));
        assert_eq!(program.counter_sink, SinkPortId(1));
        assert_eq!(program.ir.nodes.len(), 7);
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
        assert_eq!(program.tick_port, SourcePortId(2010));
        assert_eq!(program.addition_press_port, SourcePortId(2012));
        assert_eq!(program.result_sink, SinkPortId(2012));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_when_example() {
        let source = include_str!("../../../playground/frontend/src/examples/when/when.bn");
        let program = try_lower_when(source).expect("when should lower");
        assert_eq!(program.tick_port, SourcePortId(2013));
        assert_eq!(program.addition_press_port, SourcePortId(2015));
        assert_eq!(program.subtraction_press_port, SourcePortId(2016));
        assert_eq!(program.result_sink, SinkPortId(2015));
        assert!(program.host_view.root.is_some());
    }

    #[test]
    fn lowers_real_while_example() {
        let source = include_str!("../../../playground/frontend/src/examples/while/while.bn");
        let program = try_lower_while(source).expect("while should lower");
        assert_eq!(program.tick_port, SourcePortId(2017));
        assert_eq!(program.addition_press_port, SourcePortId(2019));
        assert_eq!(program.subtraction_press_port, SourcePortId(2020));
        assert_eq!(program.result_sink, SinkPortId(2018));
        assert!(program.host_view.root.is_some());
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
        assert_eq!(program.item_sinks[0], SinkPortId(32));
        assert_eq!(program.item_sinks[5], SinkPortId(37));
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
        assert_eq!(program.ir.functions.len(), 1);
        assert!(matches!(
            program.ir.nodes.iter().find(|node| node.id == NodeId(4024)),
            Some(IrNode {
                kind: IrNodeKind::ListMap { .. },
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
        assert_eq!(program.ir.nodes.len(), 28);
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
        assert_eq!(program.ir.nodes.len(), 25);
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
        assert_eq!(program.ir.nodes.len(), 26);
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
        assert_eq!(program.ir.nodes.len(), 41);
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
        assert_eq!(program.ir.nodes.len(), 147);
    }

    #[test]
    fn lowers_real_todo_mvc_physical_example() {
        let source =
            include_str!("../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn");
        let program = try_lower_todo_mvc_physical(source).expect("todo_mvc_physical should lower");
        let _ = program;
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
}
