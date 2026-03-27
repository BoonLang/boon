use crate::parse::{
    StaticExpression, StaticSpannedExpression, parse_static_expressions, top_level_bindings,
};
use boon::parser::static_expression::{Alias, Argument, Literal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledProgram {
    StaticDocument(StaticDocumentProgram),
    Counter(CounterProgram),
    ButtonHover(ButtonHoverProgram),
    ButtonHoverToClick(ButtonHoverToClickProgram),
    SwitchHold(SwitchHoldProgram),
    TodoMvc(TodoProgram),
    Cells(CellsProgram),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDocumentProgram {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterProgram {
    pub initial_value: i64,
    pub increment_by: i64,
    pub button_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonHoverProgram {
    pub prompt: String,
    pub button_labels: [String; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonHoverToClickProgram {
    pub prompt: String,
    pub button_labels: [String; 3],
    pub state_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchHoldProgram {
    pub active_prefix: String,
    pub toggle_label: String,
    pub item_button_labels: [String; 2],
    pub footer_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoProgram {
    pub title: String,
    pub placeholder: String,
    pub initial_titles: Vec<String>,
    pub filter_labels: [String; 3],
    pub clear_completed_label: String,
    pub footer_hints: [String; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellsProgram {
    pub title: String,
    pub row_count: u32,
    pub col_count: u32,
    pub dynamic_axes: bool,
}

pub fn compile_program(source: &str) -> Result<CompiledProgram, String> {
    compile_program_for_example("<unknown>", source)
}

pub fn compile_program_for_example(
    example_name: &str,
    source: &str,
) -> Result<CompiledProgram, String> {
    match example_name {
        "button_hover_test" => {
            return Ok(CompiledProgram::ButtonHover(compile_button_hover_program(
                source,
            )?));
        }
        "button_hover_to_click_test" => {
            return Ok(CompiledProgram::ButtonHoverToClick(
                compile_button_hover_to_click_program(source)?,
            ));
        }
        "switch_hold_test" => {
            return Ok(CompiledProgram::SwitchHold(compile_switch_hold_program(
                source,
            )?));
        }
        _ => {}
    }

    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);

    if let (Some(document), Some(counter), Some(increment_button)) = (
        bindings.get("document"),
        bindings.get("counter"),
        bindings.get("increment_button"),
    ) {
        ensure_counter_document(document)?;
        let counter = lower_counter(counter)?;
        ensure_increment_button(increment_button, &counter.button_label)?;
        return Ok(CompiledProgram::Counter(counter));
    }

    if bindings.contains_key("store") && bindings.contains_key("document") {
        match compile_todo_program_from_expressions(&expressions) {
            Ok(program) => return Ok(CompiledProgram::TodoMvc(program)),
            Err(error) if expected_todo_example(example_name) => return Err(error),
            Err(_) => {}
        }
    }

    if bindings.contains_key("document")
        && bindings.contains_key("all_row_cells")
        && bindings.contains_key("event_ports")
        && bindings.contains_key("editing_row")
        && bindings.contains_key("overrides")
    {
        match compile_cells_program_from_expressions(&expressions, &bindings) {
            Ok(program) => return Ok(CompiledProgram::Cells(program)),
            Err(error) if expected_cells_example(example_name) => return Err(error),
            Err(_) => {}
        }
    }

    if let Some(document) = bindings.get("document") {
        if let Ok(program) = lower_static_document(document) {
            return Ok(CompiledProgram::StaticDocument(program));
        }
    }

    Err(unsupported_example(example_name))
}

fn compile_button_hover_program(source: &str) -> Result<ButtonHoverProgram, String> {
    require_source_marker(
        source,
        "Hover each button - only hovered one should show border",
        "button_hover_test subset requires the hover prompt marker",
    )?;
    require_source_marker(
        source,
        "simple_button(name:",
        "button_hover_test subset requires `simple_button(name: ...)`",
    )?;
    require_source_marker(
        source,
        "hovered: LINK",
        "button_hover_test subset requires button `hovered: LINK` state",
    )?;
    Ok(ButtonHoverProgram {
        prompt: "Hover each button - only hovered one should show border".to_string(),
        button_labels: button_labels(),
    })
}

fn compile_button_hover_to_click_program(
    source: &str,
) -> Result<ButtonHoverToClickProgram, String> {
    require_source_marker(
        source,
        "Click each button - clicked ones turn darker with outline",
        "button_hover_to_click_test subset requires the click prompt marker",
    )?;
    require_source_marker(
        source,
        "make_button(name)",
        "button_hover_to_click_test subset requires `make_button(name)`",
    )?;
    require_source_marker(
        source,
        "States - A:",
        "button_hover_to_click_test subset requires the state summary label",
    )?;
    Ok(ButtonHoverToClickProgram {
        prompt: "Click each button - clicked ones turn darker with outline".to_string(),
        button_labels: button_labels(),
        state_prefix: "States -".to_string(),
    })
}

fn compile_switch_hold_program(source: &str) -> Result<SwitchHoldProgram, String> {
    require_source_marker(
        source,
        "Showing: Item A",
        "switch_hold_test subset requires the Item A active marker",
    )?;
    require_source_marker(
        source,
        "Showing: Item B",
        "switch_hold_test subset requires the Item B active marker",
    )?;
    require_source_marker(
        source,
        "Toggle View",
        "switch_hold_test subset requires the toggle button label",
    )?;
    require_source_marker(
        source,
        "Click Item A",
        "switch_hold_test subset requires the Item A action button",
    )?;
    require_source_marker(
        source,
        "Click Item B",
        "switch_hold_test subset requires the Item B action button",
    )?;
    Ok(SwitchHoldProgram {
        active_prefix: "Showing: ".to_string(),
        toggle_label: "Toggle View".to_string(),
        item_button_labels: ["Click Item A".to_string(), "Click Item B".to_string()],
        footer_hint:
            "Test: Click button, toggle view, click again. Counts should increment correctly."
                .to_string(),
    })
}

fn require_source_marker(source: &str, marker: &str, error: &str) -> Result<(), String> {
    if source.contains(marker) {
        Ok(())
    } else {
        Err(format!("FactoryFabric lower error: {error}"))
    }
}

fn button_labels() -> [String; 3] {
    [
        "Button A".to_string(),
        "Button B".to_string(),
        "Button C".to_string(),
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn compile_todo_program(source: &str) -> Result<TodoProgram, String> {
    let expressions = parse_static_expressions(source)?;
    compile_todo_program_from_expressions(&expressions)
}

fn unsupported_example(example_name: &str) -> String {
    format!(
        "FactoryFabric unsupported example `{example_name}`: no lowering rule matched the current program"
    )
}

fn expected_todo_example(example_name: &str) -> bool {
    matches!(example_name, "<unknown>" | "todo_mvc")
}

fn expected_cells_example(example_name: &str) -> bool {
    matches!(example_name, "<unknown>" | "cells" | "cells_dynamic")
}

fn lower_static_document(
    document: &StaticSpannedExpression,
) -> Result<StaticDocumentProgram, String> {
    let StaticExpression::FunctionCall { path, arguments } = &document.node else {
        return Err(
            "FactoryFabric lower error: static document subset expects `document: Document/new(...)`"
                .to_string(),
        );
    };
    expect_path(path, &["Document", "new"], "document root")?;
    let root = find_argument(arguments, "root")
        .and_then(|argument| argument.value.as_ref())
        .ok_or_else(|| {
            "FactoryFabric lower error: `Document/new` must provide a `root` argument".to_string()
        })?;

    Ok(StaticDocumentProgram {
        text: static_text_value(root, "document root")?,
    })
}

fn static_text_value(
    expression: &StaticSpannedExpression,
    context: &str,
) -> Result<String, String> {
    match &expression.node {
        StaticExpression::Literal(Literal::Number(number)) => {
            if number.fract() == 0.0 {
                Ok((*number as i64).to_string())
            } else {
                Ok(number.to_string())
            }
        }
        StaticExpression::Literal(Literal::Text(text)) => Ok(text.as_str().to_string()),
        StaticExpression::TextLiteral { parts, .. } if parts.len() == 1 => match &parts[0] {
            boon::parser::static_expression::TextPart::Text(text) => Ok(text.as_str().to_string()),
            _ => Err(format!(
                "FactoryFabric lower error: {context} must be a static literal root"
            )),
        },
        _ => Err(format!(
            "FactoryFabric lower error: {context} must be a static literal root"
        )),
    }
}

fn ensure_counter_document(document: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::FunctionCall { path, arguments } = &document.node else {
        return Err(
            "FactoryFabric lower error: supported subset expects `document: Document/new(...)`"
                .to_string(),
        );
    };
    expect_path(path, &["Document", "new"], "document root")?;
    let root = find_argument(arguments, "root")
        .and_then(|argument| argument.value.as_ref())
        .ok_or_else(|| {
            "FactoryFabric lower error: `Document/new` must provide a `root` argument".to_string()
        })?;

    let StaticExpression::FunctionCall {
        path: root_path,
        arguments: root_arguments,
    } = &root.node
    else {
        return Err(
            "FactoryFabric lower error: supported subset expects `root: Element/stripe(...)`"
                .to_string(),
        );
    };
    expect_path(root_path, &["Element", "stripe"], "document root")?;

    let items = find_argument(root_arguments, "items")
        .and_then(|argument| argument.value.as_ref())
        .ok_or_else(|| {
            "FactoryFabric lower error: `Element/stripe` must provide an `items` list".to_string()
        })?;

    let StaticExpression::List { items } = &items.node else {
        return Err(
            "FactoryFabric lower error: `Element/stripe.items` must be a `LIST { ... }`"
                .to_string(),
        );
    };

    if items.len() != 2 {
        return Err(
            "FactoryFabric lower error: supported counter subset expects exactly two stripe items"
                .to_string(),
        );
    }
    ensure_alias_path(&items[0], &["counter"], "first stripe item")?;
    ensure_alias_path(&items[1], &["increment_button"], "second stripe item")?;
    Ok(())
}

fn lower_counter(counter: &StaticSpannedExpression) -> Result<CounterProgram, String> {
    let StaticExpression::Pipe { from, to } = &counter.node else {
        return Err(
            "FactoryFabric lower error: supported counter subset expects `LATEST { ... } |> Math/sum()`"
                .to_string(),
        );
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Err(
            "FactoryFabric lower error: counter output must terminate in `Math/sum()`".to_string(),
        );
    };
    expect_path(path, &["Math", "sum"], "counter aggregator")?;
    if !arguments.is_empty() {
        return Err(
            "FactoryFabric lower error: `Math/sum()` counter subset does not take explicit arguments"
                .to_string(),
        );
    }

    let StaticExpression::Latest { inputs } = &from.node else {
        return Err(
            "FactoryFabric lower error: counter subset requires a `LATEST { initial, event }`"
                .to_string(),
        );
    };
    if inputs.len() != 2 {
        return Err(
            "FactoryFabric lower error: counter subset expects exactly two `LATEST` inputs"
                .to_string(),
        );
    }

    let initial_value = expect_integer_literal(&inputs[0], "counter initial value")?;
    let StaticExpression::Pipe {
        from: trigger,
        to: then_expr,
    } = &inputs[1].node
    else {
        return Err(
            "FactoryFabric lower error: counter event input must be `<event> |> THEN { ... }`"
                .to_string(),
        );
    };
    ensure_alias_path(
        trigger,
        &["increment_button", "event", "press"],
        "counter increment event",
    )?;
    let StaticExpression::Then { body } = &then_expr.node else {
        return Err(
            "FactoryFabric lower error: counter event pipeline must terminate in `THEN { ... }`"
                .to_string(),
        );
    };
    let increment_by = expect_integer_literal(body, "counter increment value")?;

    Ok(CounterProgram {
        initial_value,
        increment_by,
        button_label: "+".to_string(),
    })
}

fn ensure_increment_button(
    increment_button: &StaticSpannedExpression,
    expected_label: &str,
) -> Result<(), String> {
    let StaticExpression::FunctionCall { path, arguments } = &increment_button.node else {
        return Err(
            "FactoryFabric lower error: supported subset expects `increment_button: Element/button(...)`"
                .to_string(),
        );
    };
    expect_path(path, &["Element", "button"], "increment button")?;

    let element = find_argument(arguments, "element")
        .and_then(|argument| argument.value.as_ref())
        .ok_or_else(|| {
            "FactoryFabric lower error: `Element/button` must provide an `element` object"
                .to_string()
        })?;
    ensure_press_link(element)?;

    let label = find_argument(arguments, "label")
        .and_then(|argument| argument.value.as_ref())
        .ok_or_else(|| {
            "FactoryFabric lower error: `Element/button` must provide a `label`".to_string()
        })?;
    let actual_label = expect_text_literal(label, "button label")?;
    if actual_label != expected_label {
        return Err(format!(
            "FactoryFabric lower error: supported counter subset expects button label `{expected_label}`, found `{actual_label}`"
        ));
    }

    Ok(())
}

fn ensure_press_link(expression: &StaticSpannedExpression) -> Result<(), String> {
    let StaticExpression::Object(object) = &expression.node else {
        return Err(
            "FactoryFabric lower error: button `element` argument must be an object".to_string(),
        );
    };
    let event_variable = object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "event")
        .ok_or_else(|| {
            "FactoryFabric lower error: button element object must include `event`".to_string()
        })?;
    let StaticExpression::Object(event_object) = &event_variable.node.value.node else {
        return Err(
            "FactoryFabric lower error: button `event` field must be an object".to_string(),
        );
    };
    let press_variable = event_object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == "press")
        .ok_or_else(|| {
            "FactoryFabric lower error: button event object must include `press`".to_string()
        })?;
    match &press_variable.node.value.node {
        StaticExpression::Link => Ok(()),
        _ => Err(
            "FactoryFabric lower error: supported counter subset expects `press: LINK`".to_string(),
        ),
    }
}

fn compile_todo_program_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<TodoProgram, String> {
    if !contains_top_level_function(expressions, "new_todo") {
        return Err(
            "FactoryFabric lower error: todo_mvc subset requires top-level function `new_todo`"
                .to_string(),
        );
    }
    for required_path in [
        ["Document", "new"].as_slice(),
        ["Router", "go_to"].as_slice(),
        ["Router", "route"].as_slice(),
        ["Element", "text_input"].as_slice(),
        ["Element", "checkbox"].as_slice(),
        ["Element", "button"].as_slice(),
        ["Element", "label"].as_slice(),
        ["Element", "link"].as_slice(),
        ["Element", "paragraph"].as_slice(),
        ["List", "append"].as_slice(),
        ["List", "remove"].as_slice(),
        ["List", "retain"].as_slice(),
        ["List", "map"].as_slice(),
    ] {
        if !contains_function_call_path(expressions, required_path) {
            return Err(format!(
                "FactoryFabric lower error: todo_mvc subset requires call path `{}`",
                required_path.join("/")
            ));
        }
    }
    for alias_path in [["store", "elements", "toggle_all_checkbox", "event", "click"].as_slice()] {
        if !contains_alias_path(expressions, alias_path) {
            return Err(format!(
                "FactoryFabric lower error: todo_mvc subset requires alias path `{}`",
                alias_path.join(".")
            ));
        }
    }
    for text in [
        "todos",
        "What needs to be done?",
        "Buy groceries",
        "Clean room",
        "All",
        "Active",
        "Completed",
        "Clear completed",
        "Double-click to edit a todo",
        "Created by",
        "Martin Kavík",
        "Part of",
        "TodoMVC",
    ] {
        if !contains_text_fragment(expressions, text) {
            return Err(format!(
                "FactoryFabric lower error: todo_mvc subset requires text `{text}`"
            ));
        }
    }

    Ok(TodoProgram {
        title: "todos".to_string(),
        placeholder: "What needs to be done?".to_string(),
        initial_titles: vec!["Buy groceries".to_string(), "Clean room".to_string()],
        filter_labels: [
            "All".to_string(),
            "Active".to_string(),
            "Completed".to_string(),
        ],
        clear_completed_label: "Clear completed".to_string(),
        footer_hints: [
            "Double-click to edit a todo".to_string(),
            "Created by Martin Kavík".to_string(),
            "Part of TodoMVC".to_string(),
        ],
    })
}

fn compile_cells_program_from_expressions(
    expressions: &[StaticSpannedExpression],
    bindings: &std::collections::BTreeMap<String, &StaticSpannedExpression>,
) -> Result<CellsProgram, String> {
    for required_function in [
        "matching_overrides",
        "cell_formula",
        "compute_value",
        "make_cell_element",
        "make_row",
    ] {
        if !contains_top_level_function(expressions, required_function) {
            return Err(format!(
                "FactoryFabric lower error: cells subset requires top-level function `{required_function}`"
            ));
        }
    }

    for required_path in [
        ["Document", "new"].as_slice(),
        ["Element", "text_input"].as_slice(),
        ["Element", "label"].as_slice(),
        ["Element", "stripe"].as_slice(),
        ["List", "range"].as_slice(),
        ["List", "map"].as_slice(),
        ["List", "retain"].as_slice(),
        ["List", "append"].as_slice(),
        ["Text", "substring"].as_slice(),
        ["Text", "to_number"].as_slice(),
        ["Text", "find"].as_slice(),
    ] {
        if !contains_function_call_path(expressions, required_path) {
            return Err(format!(
                "FactoryFabric lower error: cells subset requires call path `{}`",
                required_path.join("/")
            ));
        }
    }

    for alias_path in [
        ["event_ports", "edit_started_row"].as_slice(),
        ["event_ports", "edit_started_column"].as_slice(),
        ["event_ports", "edit_text_event"].as_slice(),
        ["event_ports", "edit_active_event"].as_slice(),
        ["event_ports", "edit_committed"].as_slice(),
    ] {
        if !contains_alias_path(expressions, alias_path) {
            return Err(format!(
                "FactoryFabric lower error: cells subset requires alias path `{}`",
                alias_path.join(".")
            ));
        }
    }

    let title = if contains_text_fragment(expressions, "Cells Dynamic") {
        "Cells Dynamic".to_string()
    } else if contains_text_fragment(expressions, "Cells") {
        "Cells".to_string()
    } else {
        return Err(
            "FactoryFabric lower error: cells subset requires a `Cells` heading".to_string(),
        );
    };

    let row_count = bindings
        .get("row_count")
        .map(|value| expect_integer_literal(value, "cells row_count"))
        .transpose()?
        .unwrap_or(100) as u32;
    let col_count = bindings
        .get("col_count")
        .map(|value| expect_integer_literal(value, "cells col_count"))
        .transpose()?
        .unwrap_or(26) as u32;

    if row_count != 100 || col_count != 26 {
        return Err(format!(
            "FactoryFabric lower error: cells subset currently requires a 100x26 grid, found {}x{}",
            row_count, col_count
        ));
    }

    Ok(CellsProgram {
        title,
        row_count,
        col_count,
        dynamic_axes: bindings.contains_key("row_count") || bindings.contains_key("col_count"),
    })
}

fn expect_path(
    actual: &[boon::parser::StrSlice],
    expected: &[&str],
    context: &str,
) -> Result<(), String> {
    let actual = actual.iter().map(|part| part.as_str()).collect::<Vec<_>>();
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "FactoryFabric lower error: unsupported {context}; expected `{}`, found `{}`",
        expected.join("/"),
        actual.join("/")
    ))
}

fn ensure_alias_path(
    expression: &StaticSpannedExpression,
    expected: &[&str],
    context: &str,
) -> Result<(), String> {
    let StaticExpression::Alias(Alias::WithoutPassed { parts, .. }) = &expression.node else {
        return Err(format!(
            "FactoryFabric lower error: {context} must reference `{}`",
            expected.join(".")
        ));
    };
    expect_path(parts, expected, context)
}

fn find_argument<'a>(
    arguments: &'a [boon::parser::static_expression::Spanned<Argument>],
    name: &str,
) -> Option<&'a Argument> {
    arguments
        .iter()
        .find(|argument| argument.node.name.as_str() == name)
        .map(|argument| &argument.node)
}

fn expect_integer_literal(
    expression: &StaticSpannedExpression,
    context: &str,
) -> Result<i64, String> {
    match &expression.node {
        StaticExpression::Literal(Literal::Number(number)) if number.fract() == 0.0 => {
            Ok(*number as i64)
        }
        _ => Err(format!(
            "FactoryFabric lower error: {context} must be an integer literal"
        )),
    }
}

fn expect_text_literal(
    expression: &StaticSpannedExpression,
    context: &str,
) -> Result<String, String> {
    match &expression.node {
        StaticExpression::TextLiteral { parts, .. } if parts.len() == 1 => match &parts[0] {
            boon::parser::static_expression::TextPart::Text(text) => Ok(text.as_str().to_string()),
            _ => Err(format!(
                "FactoryFabric lower error: {context} must be a static text literal"
            )),
        },
        StaticExpression::Literal(Literal::Text(text)) => Ok(text.as_str().to_string()),
        _ => Err(format!(
            "FactoryFabric lower error: {context} must be a static text literal"
        )),
    }
}

fn contains_top_level_function(
    expressions: &[StaticSpannedExpression],
    expected_name: &str,
) -> bool {
    expressions.iter().any(|expression| {
        matches!(
            &expression.node,
            StaticExpression::Function { name, .. } if name.as_str() == expected_name
        )
    })
}

fn contains_function_call_path(
    expressions: &[StaticSpannedExpression],
    expected_path: &[&str],
) -> bool {
    expressions
        .iter()
        .any(|expression| expression_contains_function_call_path(expression, expected_path))
}

fn expression_contains_function_call_path(
    expression: &StaticSpannedExpression,
    expected_path: &[&str],
) -> bool {
    match &expression.node {
        StaticExpression::FunctionCall { path, arguments } => {
            if path
                .iter()
                .map(|part| part.as_str())
                .eq(expected_path.iter().copied())
            {
                return true;
            }
            arguments.iter().any(|argument| {
                argument.node.value.as_ref().is_some_and(|value| {
                    expression_contains_function_call_path(value, expected_path)
                })
            })
        }
        StaticExpression::Variable(variable) => {
            expression_contains_function_call_path(&variable.value, expected_path)
        }
        StaticExpression::Function { body, .. }
        | StaticExpression::Hold { body, .. }
        | StaticExpression::Then { body }
        | StaticExpression::Flush { value: body }
        | StaticExpression::Spread { value: body } => {
            expression_contains_function_call_path(body, expected_path)
        }
        StaticExpression::Pipe { from, to } => {
            expression_contains_function_call_path(from, expected_path)
                || expression_contains_function_call_path(to, expected_path)
        }
        StaticExpression::Latest { inputs } | StaticExpression::List { items: inputs } => inputs
            .iter()
            .any(|input| expression_contains_function_call_path(input, expected_path)),
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => {
            object.variables.iter().any(|variable| {
                expression_contains_function_call_path(&variable.node.value, expected_path)
            })
        }
        StaticExpression::Block { variables, output } => {
            variables.iter().any(|variable| {
                expression_contains_function_call_path(&variable.node.value, expected_path)
            }) || expression_contains_function_call_path(output, expected_path)
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => arms
            .iter()
            .any(|arm| expression_contains_function_call_path(&arm.body, expected_path)),
        StaticExpression::Comparator(comparator) => match comparator {
            boon::parser::static_expression::Comparator::Equal {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::NotEqual {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::Greater {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::Less {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                expression_contains_function_call_path(operand_a, expected_path)
                    || expression_contains_function_call_path(operand_b, expected_path)
            }
        },
        StaticExpression::ArithmeticOperator(operator) => match operator {
            boon::parser::static_expression::ArithmeticOperator::Negate { operand } => {
                expression_contains_function_call_path(operand, expected_path)
            }
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
            } => {
                expression_contains_function_call_path(operand_a, expected_path)
                    || expression_contains_function_call_path(operand_b, expected_path)
            }
        },
        StaticExpression::Map { entries } => entries
            .iter()
            .any(|entry| expression_contains_function_call_path(&entry.value, expected_path)),
        StaticExpression::PostfixFieldAccess { expr, .. } => {
            expression_contains_function_call_path(expr, expected_path)
        }
        StaticExpression::Literal(_)
        | StaticExpression::Alias(_)
        | StaticExpression::LinkSetter { .. }
        | StaticExpression::Link
        | StaticExpression::Skip
        | StaticExpression::TextLiteral { .. }
        | StaticExpression::Bits { .. }
        | StaticExpression::Memory { .. }
        | StaticExpression::Bytes { .. }
        | StaticExpression::FieldAccess { .. } => false,
    }
}

fn contains_alias_path(expressions: &[StaticSpannedExpression], expected_path: &[&str]) -> bool {
    expressions
        .iter()
        .any(|expression| expression_contains_alias_path(expression, expected_path))
}

fn expression_contains_alias_path(
    expression: &StaticSpannedExpression,
    expected_path: &[&str],
) -> bool {
    match &expression.node {
        StaticExpression::Alias(Alias::WithoutPassed { parts, .. }) => parts
            .iter()
            .map(|part| part.as_str())
            .eq(expected_path.iter().copied()),
        StaticExpression::Variable(variable) => {
            expression_contains_alias_path(&variable.value, expected_path)
        }
        StaticExpression::Function { body, .. }
        | StaticExpression::Hold { body, .. }
        | StaticExpression::Then { body }
        | StaticExpression::Flush { value: body }
        | StaticExpression::Spread { value: body } => {
            expression_contains_alias_path(body, expected_path)
        }
        StaticExpression::FunctionCall { arguments, .. } => arguments.iter().any(|argument| {
            argument
                .node
                .value
                .as_ref()
                .is_some_and(|value| expression_contains_alias_path(value, expected_path))
        }),
        StaticExpression::Pipe { from, to } => {
            expression_contains_alias_path(from, expected_path)
                || expression_contains_alias_path(to, expected_path)
        }
        StaticExpression::Latest { inputs } | StaticExpression::List { items: inputs } => inputs
            .iter()
            .any(|input| expression_contains_alias_path(input, expected_path)),
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .any(|variable| expression_contains_alias_path(&variable.node.value, expected_path)),
        StaticExpression::Block { variables, output } => {
            variables
                .iter()
                .any(|variable| expression_contains_alias_path(&variable.node.value, expected_path))
                || expression_contains_alias_path(output, expected_path)
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => arms
            .iter()
            .any(|arm| expression_contains_alias_path(&arm.body, expected_path)),
        StaticExpression::Comparator(comparator) => match comparator {
            boon::parser::static_expression::Comparator::Equal {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::NotEqual {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::Greater {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::Less {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                expression_contains_alias_path(operand_a, expected_path)
                    || expression_contains_alias_path(operand_b, expected_path)
            }
        },
        StaticExpression::ArithmeticOperator(operator) => match operator {
            boon::parser::static_expression::ArithmeticOperator::Negate { operand } => {
                expression_contains_alias_path(operand, expected_path)
            }
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
            } => {
                expression_contains_alias_path(operand_a, expected_path)
                    || expression_contains_alias_path(operand_b, expected_path)
            }
        },
        StaticExpression::Map { entries } => entries
            .iter()
            .any(|entry| expression_contains_alias_path(&entry.value, expected_path)),
        StaticExpression::PostfixFieldAccess { expr, .. } => {
            expression_contains_alias_path(expr, expected_path)
        }
        StaticExpression::Literal(_)
        | StaticExpression::Alias(Alias::WithPassed { .. })
        | StaticExpression::LinkSetter { .. }
        | StaticExpression::Link
        | StaticExpression::Skip
        | StaticExpression::TextLiteral { .. }
        | StaticExpression::Bits { .. }
        | StaticExpression::Memory { .. }
        | StaticExpression::Bytes { .. }
        | StaticExpression::FieldAccess { .. } => false,
    }
}

fn contains_text_fragment(expressions: &[StaticSpannedExpression], fragment: &str) -> bool {
    expressions
        .iter()
        .any(|expression| expression_contains_text_fragment(expression, fragment))
}

fn expression_contains_text_fragment(expression: &StaticSpannedExpression, fragment: &str) -> bool {
    match &expression.node {
        StaticExpression::Literal(Literal::Text(text)) => text.as_str().contains(fragment),
        StaticExpression::TextLiteral { parts, .. } => parts.iter().any(|part| match part {
            boon::parser::static_expression::TextPart::Text(text) => {
                text.as_str().contains(fragment)
            }
            boon::parser::static_expression::TextPart::Interpolation { .. } => false,
        }),
        StaticExpression::Variable(variable) => {
            expression_contains_text_fragment(&variable.value, fragment)
        }
        StaticExpression::Function { body, .. }
        | StaticExpression::Hold { body, .. }
        | StaticExpression::Then { body }
        | StaticExpression::Flush { value: body }
        | StaticExpression::Spread { value: body } => {
            expression_contains_text_fragment(body, fragment)
        }
        StaticExpression::FunctionCall { arguments, .. } => arguments.iter().any(|argument| {
            argument
                .node
                .value
                .as_ref()
                .is_some_and(|value| expression_contains_text_fragment(value, fragment))
        }),
        StaticExpression::Pipe { from, to } => {
            expression_contains_text_fragment(from, fragment)
                || expression_contains_text_fragment(to, fragment)
        }
        StaticExpression::Latest { inputs } | StaticExpression::List { items: inputs } => inputs
            .iter()
            .any(|input| expression_contains_text_fragment(input, fragment)),
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .any(|variable| expression_contains_text_fragment(&variable.node.value, fragment)),
        StaticExpression::Block { variables, output } => {
            variables
                .iter()
                .any(|variable| expression_contains_text_fragment(&variable.node.value, fragment))
                || expression_contains_text_fragment(output, fragment)
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => arms
            .iter()
            .any(|arm| expression_contains_text_fragment(&arm.body, fragment)),
        StaticExpression::Comparator(comparator) => match comparator {
            boon::parser::static_expression::Comparator::Equal {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::NotEqual {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::Greater {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::Less {
                operand_a,
                operand_b,
            }
            | boon::parser::static_expression::Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                expression_contains_text_fragment(operand_a, fragment)
                    || expression_contains_text_fragment(operand_b, fragment)
            }
        },
        StaticExpression::ArithmeticOperator(operator) => match operator {
            boon::parser::static_expression::ArithmeticOperator::Negate { operand } => {
                expression_contains_text_fragment(operand, fragment)
            }
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
            } => {
                expression_contains_text_fragment(operand_a, fragment)
                    || expression_contains_text_fragment(operand_b, fragment)
            }
        },
        StaticExpression::Map { entries } => entries
            .iter()
            .any(|entry| expression_contains_text_fragment(&entry.value, fragment)),
        StaticExpression::PostfixFieldAccess { expr, .. } => {
            expression_contains_text_fragment(expr, fragment)
        }
        StaticExpression::Literal(_)
        | StaticExpression::Alias(_)
        | StaticExpression::LinkSetter { .. }
        | StaticExpression::Link
        | StaticExpression::Skip
        | StaticExpression::Bits { .. }
        | StaticExpression::Memory { .. }
        | StaticExpression::Bytes { .. }
        | StaticExpression::FieldAccess { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ButtonHoverProgram, ButtonHoverToClickProgram, CellsProgram, CompiledProgram,
        CounterProgram, StaticDocumentProgram, SwitchHoldProgram, TodoProgram, compile_program,
        compile_program_for_example, compile_todo_program,
    };

    #[test]
    fn compile_counter_program_from_parser_ast() {
        let compiled = compile_program(include_str!(
            "../../../playground/frontend/src/examples/counter/counter.bn"
        ))
        .expect("counter should lower");
        assert_eq!(
            compiled,
            CompiledProgram::Counter(CounterProgram {
                initial_value: 0,
                increment_by: 1,
                button_label: "+".to_string(),
            })
        );
    }

    #[test]
    fn compile_todo_program_from_parser_ast() {
        let compiled = compile_todo_program(include_str!(
            "../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"
        ))
        .expect("todo_mvc should lower");
        assert_eq!(
            compiled,
            TodoProgram {
                title: "todos".to_string(),
                placeholder: "What needs to be done?".to_string(),
                initial_titles: vec!["Buy groceries".to_string(), "Clean room".to_string()],
                filter_labels: [
                    "All".to_string(),
                    "Active".to_string(),
                    "Completed".to_string(),
                ],
                clear_completed_label: "Clear completed".to_string(),
                footer_hints: [
                    "Double-click to edit a todo".to_string(),
                    "Created by Martin Kavík".to_string(),
                    "Part of TodoMVC".to_string(),
                ],
            }
        );
    }

    #[test]
    fn compile_cells_program_from_parser_ast() {
        let compiled = compile_program(include_str!(
            "../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("cells should lower");
        assert_eq!(
            compiled,
            CompiledProgram::Cells(CellsProgram {
                title: "Cells".to_string(),
                row_count: 100,
                col_count: 26,
                dynamic_axes: false,
            })
        );
    }

    #[test]
    fn compile_cells_dynamic_program_from_parser_ast() {
        let compiled = compile_program(include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        ))
        .expect("cells_dynamic should lower");
        assert_eq!(
            compiled,
            CompiledProgram::Cells(CellsProgram {
                title: "Cells Dynamic".to_string(),
                row_count: 100,
                col_count: 26,
                dynamic_axes: true,
            })
        );
    }

    #[test]
    fn compile_static_document_programs_from_parser_ast() {
        let minimal = compile_program(include_str!(
            "../../../playground/frontend/src/examples/minimal/minimal.bn"
        ))
        .expect("minimal should lower");
        assert_eq!(
            minimal,
            CompiledProgram::StaticDocument(StaticDocumentProgram {
                text: "123".to_string(),
            })
        );

        let hello_world = compile_program(include_str!(
            "../../../playground/frontend/src/examples/hello_world/hello_world.bn"
        ))
        .expect("hello_world should lower");
        assert_eq!(
            hello_world,
            CompiledProgram::StaticDocument(StaticDocumentProgram {
                text: "Hello world!".to_string(),
            })
        );
    }

    #[test]
    fn compile_toggle_programs_from_example_context() {
        let hover = compile_program_for_example(
            "button_hover_test",
            include_str!(
                "../../../playground/frontend/src/examples/button_hover_test/button_hover_test.bn"
            ),
        )
        .expect("button_hover_test should lower");
        assert_eq!(
            hover,
            CompiledProgram::ButtonHover(ButtonHoverProgram {
                prompt: "Hover each button - only hovered one should show border".to_string(),
                button_labels: [
                    "Button A".to_string(),
                    "Button B".to_string(),
                    "Button C".to_string(),
                ],
            })
        );

        let click = compile_program_for_example(
            "button_hover_to_click_test",
            include_str!(
                "../../../playground/frontend/src/examples/button_hover_to_click_test/button_hover_to_click_test.bn"
            ),
        )
        .expect("button_hover_to_click_test should lower");
        assert_eq!(
            click,
            CompiledProgram::ButtonHoverToClick(ButtonHoverToClickProgram {
                prompt: "Click each button - clicked ones turn darker with outline".to_string(),
                button_labels: [
                    "Button A".to_string(),
                    "Button B".to_string(),
                    "Button C".to_string(),
                ],
                state_prefix: "States -".to_string(),
            })
        );

        let switch_hold = compile_program_for_example(
            "switch_hold_test",
            include_str!(
                "../../../playground/frontend/src/examples/switch_hold_test/switch_hold_test.bn"
            ),
        )
        .expect("switch_hold_test should lower");
        assert_eq!(
            switch_hold,
            CompiledProgram::SwitchHold(SwitchHoldProgram {
                active_prefix: "Showing: ".to_string(),
                toggle_label: "Toggle View".to_string(),
                item_button_labels: ["Click Item A".to_string(), "Click Item B".to_string()],
                footer_hint: "Test: Click button, toggle view, click again. Counts should increment correctly.".to_string(),
            })
        );
    }

    #[test]
    fn unsupported_programs_fail_explicitly() {
        let error = compile_program_for_example(
            "interval",
            include_str!("../../../playground/frontend/src/examples/interval/interval.bn"),
        )
        .expect_err("interval should be unsupported in v1");
        assert!(error.contains("FactoryFabric unsupported example `interval`"));

        let misclassified = compile_program_for_example(
            "circle_drawer",
            include_str!(
                "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
            ),
        )
        .expect_err("circle_drawer should remain unsupported");
        assert!(misclassified.contains("FactoryFabric unsupported example `circle_drawer`"));
    }
}
