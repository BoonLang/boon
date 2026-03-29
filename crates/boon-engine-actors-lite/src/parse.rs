use crate::ir::{IrNodePersistence, NodeId, PersistKind, PersistPolicy};
use boon::parser::{
    Input as _, Parser as _, Persistence, PersistenceId, PersistenceStatus, SourceCode, StrSlice,
    Token, lexer, parser, reset_expression_depth, resolve_references, span_at, static_expression,
};
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
use boon::parser::{Spanned as ParsedSpanned, resolve_persistence};

pub type StaticExpression = static_expression::Expression;
pub type StaticSpannedExpression = static_expression::Spanned<StaticExpression>;

pub fn parse_static_expressions(source: &str) -> Result<Vec<StaticSpannedExpression>, String> {
    let source_code = SourceCode::new(source.to_string());
    let parse_source = source_code.clone();
    let source = parse_source.as_str();
    let (tokens, lex_errors) = lexer().parse(source).into_output_errors();
    if let Some(error) = lex_errors.into_iter().next() {
        return Err(format!("lex error: {error}"));
    }
    let mut tokens = tokens.ok_or_else(|| "lex error: no tokens produced".to_string())?;
    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    reset_expression_depth();
    let (ast, parse_errors) = parser()
        .parse(tokens.map(
            span_at(source.len()),
            |boon::parser::Spanned {
                 node,
                 span,
                 persistence: _,
             }| (node, span),
        ))
        .into_output_errors();
    if let Some(error) = parse_errors.into_iter().next() {
        return Err(format!("parse error: {error}"));
    }
    let ast = ast.ok_or_else(|| "parse error: no AST produced".to_string())?;
    let ast = resolve_references(ast).map_err(|errors| {
        errors.into_iter().next().map_or_else(
            || "reference error".to_string(),
            |error| format!("reference error: {error}"),
        )
    })?;
    #[cfg(target_arch = "wasm32")]
    let ast = {
        let (ast, _new_span_id_pairs, _changed_variable_ids) = resolve_persistence(
            ast,
            None::<Vec<ParsedSpanned<boon::parser::Expression<'static>>>>,
            "",
        )
        .map_err(|errors| {
            errors.into_iter().next().map_or_else(
                || "persistence error".to_string(),
                |error| format!("persistence error: {error}"),
            )
        })?;
        ast
    };

    let mut expressions = static_expression::convert_expressions(source_code, ast);
    #[cfg(not(target_arch = "wasm32"))]
    assign_synthetic_persistence(&mut expressions);

    Ok(expressions)
}

#[cfg(not(target_arch = "wasm32"))]
fn assign_synthetic_persistence(expressions: &mut [StaticSpannedExpression]) {
    for (index, expression) in expressions.iter_mut().enumerate() {
        assign_expression_persistence(
            expression,
            PersistenceId::new().with_child_index(index as u32),
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn assign_expression_persistence(expression: &mut StaticSpannedExpression, id: PersistenceId) {
    expression.persistence = Some(Persistence {
        id,
        status: PersistenceStatus::NewOrChanged,
    });

    match &mut expression.node {
        StaticExpression::Variable(variable) => {
            assign_expression_persistence(&mut variable.value, id.with_child_index(0));
            variable.value_changed = matches!(
                variable.value.persistence,
                Some(Persistence {
                    status: PersistenceStatus::NewOrChanged,
                    ..
                })
            );
        }
        StaticExpression::Literal(_)
        | StaticExpression::Alias(_)
        | StaticExpression::Link
        | StaticExpression::Skip
        | StaticExpression::FieldAccess { .. } => {}
        StaticExpression::List { items } | StaticExpression::Latest { inputs: items } => {
            for (index, item) in items.iter_mut().enumerate() {
                assign_expression_persistence(item, id.with_child_index(index as u32));
            }
        }
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => {
            for (index, variable) in object.variables.iter_mut().enumerate() {
                variable.persistence = Some(Persistence {
                    id: id.with_child_index(index as u32),
                    status: PersistenceStatus::NewOrChanged,
                });
                assign_expression_persistence(
                    &mut variable.node.value,
                    id.with_child_index((index as u32) + 100),
                );
                variable.node.value_changed = matches!(
                    variable.node.value.persistence,
                    Some(Persistence {
                        status: PersistenceStatus::NewOrChanged,
                        ..
                    })
                );
            }
        }
        StaticExpression::Map { entries } => {
            for (index, entry) in entries.iter_mut().enumerate() {
                entry.key.persistence = Some(Persistence {
                    id: id.with_child_index(index as u32),
                    status: PersistenceStatus::NewOrChanged,
                });
                assign_expression_persistence(
                    &mut entry.value,
                    id.with_child_index((index as u32) + 100),
                );
            }
        }
        StaticExpression::Function {
            parameters, body, ..
        } => {
            for (index, parameter) in parameters.iter_mut().enumerate() {
                parameter.persistence = Some(Persistence {
                    id: id.with_child_index(index as u32),
                    status: PersistenceStatus::NewOrChanged,
                });
            }
            assign_expression_persistence(body, id.with_child_index(100));
        }
        StaticExpression::FunctionCall { arguments, .. } => {
            for (index, argument) in arguments.iter_mut().enumerate() {
                argument.persistence = Some(Persistence {
                    id: id.with_child_index(index as u32),
                    status: PersistenceStatus::NewOrChanged,
                });
                if let Some(value) = argument.node.value.as_mut() {
                    assign_expression_persistence(value, id.with_child_index((index as u32) + 100));
                }
            }
        }
        StaticExpression::LinkSetter { alias } => {
            alias.persistence = Some(Persistence {
                id: id.with_child_index(0),
                status: PersistenceStatus::NewOrChanged,
            });
        }
        StaticExpression::Hold { body, .. } | StaticExpression::Then { body } => {
            assign_expression_persistence(body, id.with_child_index(0));
        }
        StaticExpression::Flush { value } | StaticExpression::Spread { value } => {
            assign_expression_persistence(value, id.with_child_index(0));
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => {
            for (index, arm) in arms.iter_mut().enumerate() {
                assign_expression_persistence(&mut arm.body, id.with_child_index(index as u32));
            }
        }
        StaticExpression::Pipe { from, to } => {
            assign_expression_persistence(from, id.with_child_index(0));
            assign_expression_persistence(to, id.with_child_index(1));
        }
        StaticExpression::Block { variables, output } => {
            for (index, variable) in variables.iter_mut().enumerate() {
                variable.persistence = Some(Persistence {
                    id: id.with_child_index(index as u32),
                    status: PersistenceStatus::NewOrChanged,
                });
                assign_expression_persistence(
                    &mut variable.node.value,
                    id.with_child_index((index as u32) + 100),
                );
                variable.node.value_changed = matches!(
                    variable.node.value.persistence,
                    Some(Persistence {
                        status: PersistenceStatus::NewOrChanged,
                        ..
                    })
                );
            }
            assign_expression_persistence(output, id.with_child_index(200));
        }
        StaticExpression::Comparator(comparator) => match comparator {
            static_expression::Comparator::Equal {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::NotEqual {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::Greater {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::Less {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                assign_expression_persistence(operand_a, id.with_child_index(0));
                assign_expression_persistence(operand_b, id.with_child_index(1));
            }
        },
        StaticExpression::ArithmeticOperator(operator) => match operator {
            static_expression::ArithmeticOperator::Negate { operand } => {
                assign_expression_persistence(operand, id.with_child_index(0));
            }
            static_expression::ArithmeticOperator::Add {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => {
                assign_expression_persistence(operand_a, id.with_child_index(0));
                assign_expression_persistence(operand_b, id.with_child_index(1));
            }
        },
        StaticExpression::TextLiteral { .. } => {}
        StaticExpression::Bits { size } | StaticExpression::Memory { address: size } => {
            assign_expression_persistence(size, id.with_child_index(0));
        }
        StaticExpression::Bytes { data } => {
            for (index, item) in data.iter_mut().enumerate() {
                assign_expression_persistence(item, id.with_child_index(index as u32));
            }
        }
        StaticExpression::PostfixFieldAccess { expr, .. } => {
            assign_expression_persistence(expr, id.with_child_index(0));
        }
    }
}

pub fn top_level_bindings<'a>(
    expressions: &'a [StaticSpannedExpression],
) -> BTreeMap<String, &'a StaticSpannedExpression> {
    expressions
        .iter()
        .filter_map(|expression| match &expression.node {
            StaticExpression::Variable(variable) => {
                Some((variable.name.as_str().to_string(), &variable.value))
            }
            _ => None,
        })
        .collect()
}

pub fn binding_at_path<'a>(
    bindings: &'a BTreeMap<String, &'a StaticSpannedExpression>,
    path: &[&str],
) -> Option<&'a StaticSpannedExpression> {
    let (root, fields) = path.split_first()?;
    let mut expression = *bindings.get(*root)?;
    for field in fields {
        expression = object_field_binding(expression, field)?;
    }
    Some(expression)
}

fn object_field_binding<'a>(
    expression: &'a StaticSpannedExpression,
    field: &str,
) -> Option<&'a StaticSpannedExpression> {
    match &expression.node {
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .find(|variable| variable.node.name.as_str() == field)
            .map(|variable| &variable.node.value),
        _ => None,
    }
}

pub fn require_top_level_bindings(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    subset: &str,
    names: &[&str],
) -> Result<(), String> {
    for name in names {
        if !bindings.contains_key(*name) {
            return Err(format!("{subset} subset requires top-level `{name}`"));
        }
    }
    Ok(())
}

pub fn require_binding_at_path<'a>(
    bindings: &'a BTreeMap<String, &'a StaticSpannedExpression>,
    subset: &str,
    path: &[&str],
) -> Result<&'a StaticSpannedExpression, String> {
    binding_at_path(bindings, path)
        .ok_or_else(|| format!("{subset} subset requires binding `{}`", path.join(".")))
}

pub fn require_hold_binding_at_path<'a>(
    bindings: &'a BTreeMap<String, &'a StaticSpannedExpression>,
    subset: &str,
    path: &[&str],
) -> Result<&'a StaticSpannedExpression, String> {
    let binding = require_binding_at_path(bindings, subset, path)?;
    if !contains_hold_expression(std::slice::from_ref(binding)) {
        return Err(format!(
            "{subset} subset requires hold binding `{}`",
            path.join(".")
        ));
    }
    Ok(binding)
}

pub fn persist_entry_for_binding(
    binding: Option<&StaticSpannedExpression>,
    node: NodeId,
    local_slot: u32,
    persist_kind: PersistKind,
) -> Option<IrNodePersistence> {
    binding.and_then(|expression| {
        expression.persistence.map(|persistence| IrNodePersistence {
            node,
            policy: PersistPolicy::Durable {
                root_key: persistence.id,
                local_slot,
                persist_kind,
            },
        })
    })
}

pub fn persist_entries_for_binding(
    binding: Option<&StaticSpannedExpression>,
    node: NodeId,
    local_slot: u32,
    persist_kind: PersistKind,
) -> Vec<IrNodePersistence> {
    persist_entry_for_binding(binding, node, local_slot, persist_kind)
        .into_iter()
        .collect()
}

pub fn persist_entry_for_path(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    path: &[&str],
    node: NodeId,
    local_slot: u32,
    persist_kind: PersistKind,
) -> Option<IrNodePersistence> {
    persist_entry_for_binding(
        binding_at_path(bindings, path),
        node,
        local_slot,
        persist_kind,
    )
}

pub fn persist_entries_for_path(
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

pub fn contains_top_level_function(
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

pub fn contains_function_call_path(
    expressions: &[StaticSpannedExpression],
    expected_path: &[&str],
) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            matches!(
                &expression.node,
                StaticExpression::FunctionCall { path, .. } if path_matches(path, expected_path)
            )
        })
    })
}

pub fn contains_alias_path(
    expressions: &[StaticSpannedExpression],
    expected_path: &[&str],
) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            matches!(
                &expression.node,
                StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
                    if path_matches(parts, expected_path)
            )
        })
    })
}

pub fn contains_hold_expression(expressions: &[StaticSpannedExpression]) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            matches!(&expression.node, StaticExpression::Hold { .. })
        })
    })
}

pub fn contains_latest_expression(expressions: &[StaticSpannedExpression]) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            matches!(&expression.node, StaticExpression::Latest { .. })
        })
    })
}

pub fn contains_then_expression(expressions: &[StaticSpannedExpression]) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            matches!(&expression.node, StaticExpression::Then { .. })
        })
    })
}

pub fn contains_when_expression(expressions: &[StaticSpannedExpression]) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            matches!(&expression.node, StaticExpression::When { .. })
        })
    })
}

pub fn contains_while_expression(expressions: &[StaticSpannedExpression]) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            matches!(&expression.node, StaticExpression::While { .. })
        })
    })
}

pub fn contains_text_fragment(
    expressions: &[StaticSpannedExpression],
    expected_fragment: &str,
) -> bool {
    expressions.iter().any(|expression| {
        any_expression_match(expression, &|expression| {
            expression_contains_text_fragment(expression, expected_fragment)
        })
    })
}

pub fn require_top_level_functions(
    expressions: &[StaticSpannedExpression],
    subset: &str,
    names: &[&str],
) -> Result<(), String> {
    for name in names {
        if !contains_top_level_function(expressions, name) {
            return Err(format!(
                "{subset} subset requires top-level function `{name}`"
            ));
        }
    }
    Ok(())
}

pub fn require_function_call_paths(
    expressions: &[StaticSpannedExpression],
    subset: &str,
    paths: &[&[&str]],
) -> Result<(), String> {
    for path in paths {
        if !contains_function_call_path(expressions, path) {
            return Err(format!(
                "{subset} subset requires function path `{}`",
                path.join("/")
            ));
        }
    }
    Ok(())
}

pub fn require_alias_paths(
    expressions: &[StaticSpannedExpression],
    subset: &str,
    paths: &[&[&str]],
) -> Result<(), String> {
    for path in paths {
        if !contains_alias_path(expressions, path) {
            return Err(format!(
                "{subset} subset requires alias path `{}`",
                path.join(".")
            ));
        }
    }
    Ok(())
}

pub fn require_text_fragments(
    expressions: &[StaticSpannedExpression],
    subset: &str,
    fragments: &[&str],
) -> Result<(), String> {
    for fragment in fragments {
        if !contains_text_fragment(expressions, fragment) {
            return Err(format!("{subset} subset requires text `{fragment}`"));
        }
    }
    Ok(())
}

fn any_expression_match(
    expression: &StaticSpannedExpression,
    predicate: &impl Fn(&StaticSpannedExpression) -> bool,
) -> bool {
    if predicate(expression) {
        return true;
    }

    match &expression.node {
        StaticExpression::Variable(variable) => any_expression_match(&variable.value, predicate),
        StaticExpression::Literal(_)
        | StaticExpression::Alias(_)
        | StaticExpression::Link
        | StaticExpression::Skip
        | StaticExpression::FieldAccess { .. } => false,
        StaticExpression::List { items } | StaticExpression::Latest { inputs: items } => items
            .iter()
            .any(|item| any_expression_match(item, predicate)),
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .any(|variable| any_expression_match(&variable.node.value, predicate)),
        StaticExpression::Map { entries } => entries
            .iter()
            .any(|entry| any_expression_match(&entry.value, predicate)),
        StaticExpression::Function { body, .. } => any_expression_match(body, predicate),
        StaticExpression::FunctionCall { arguments, .. } => arguments
            .iter()
            .filter_map(|argument| argument.node.value.as_ref())
            .any(|value| any_expression_match(value, predicate)),
        StaticExpression::LinkSetter { .. } => false,
        StaticExpression::Hold { body, .. } | StaticExpression::Then { body } => {
            any_expression_match(body, predicate)
        }
        StaticExpression::Flush { value } | StaticExpression::Spread { value } => {
            any_expression_match(value, predicate)
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => arms
            .iter()
            .any(|arm| any_expression_match(&arm.body, predicate)),
        StaticExpression::Pipe { from, to } => {
            any_expression_match(from, predicate) || any_expression_match(to, predicate)
        }
        StaticExpression::Block { variables, output } => {
            variables
                .iter()
                .any(|variable| any_expression_match(&variable.node.value, predicate))
                || any_expression_match(output, predicate)
        }
        StaticExpression::Comparator(comparator) => comparator_operands(comparator)
            .into_iter()
            .any(|operand| any_expression_match(operand, predicate)),
        StaticExpression::ArithmeticOperator(operator) => operator_operands(operator)
            .into_iter()
            .any(|operand| any_expression_match(operand, predicate)),
        StaticExpression::TextLiteral { .. } => false,
        StaticExpression::Bits { size } | StaticExpression::Memory { address: size } => {
            any_expression_match(size, predicate)
        }
        StaticExpression::Bytes { data } => data
            .iter()
            .any(|item| any_expression_match(item, predicate)),
        StaticExpression::PostfixFieldAccess { expr, .. } => any_expression_match(expr, predicate),
    }
}

fn comparator_operands(
    comparator: &static_expression::Comparator,
) -> Vec<&StaticSpannedExpression> {
    match comparator {
        static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        }
        | static_expression::Comparator::NotEqual {
            operand_a,
            operand_b,
        }
        | static_expression::Comparator::Greater {
            operand_a,
            operand_b,
        }
        | static_expression::Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        }
        | static_expression::Comparator::Less {
            operand_a,
            operand_b,
        }
        | static_expression::Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => vec![operand_a, operand_b],
    }
}

fn operator_operands(
    operator: &static_expression::ArithmeticOperator,
) -> Vec<&StaticSpannedExpression> {
    match operator {
        static_expression::ArithmeticOperator::Negate { operand } => vec![operand],
        static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }
        | static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }
        | static_expression::ArithmeticOperator::Multiply {
            operand_a,
            operand_b,
        }
        | static_expression::ArithmeticOperator::Divide {
            operand_a,
            operand_b,
        } => vec![operand_a, operand_b],
    }
}

fn expression_contains_text_fragment(
    expression: &StaticSpannedExpression,
    expected_fragment: &str,
) -> bool {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            text.as_str().contains(expected_fragment)
        }
        StaticExpression::TextLiteral { parts, .. } => parts.iter().any(|part| match part {
            static_expression::TextPart::Text(text) => text.as_str().contains(expected_fragment),
            static_expression::TextPart::Interpolation { .. } => false,
        }),
        _ => false,
    }
}

fn path_matches(path: &[StrSlice], expected_path: &[&str]) -> bool {
    path.len() == expected_path.len()
        && path
            .iter()
            .zip(expected_path.iter())
            .all(|(actual, expected)| actual.as_str() == *expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_counter_example_into_bindings() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let expressions = parse_static_expressions(source).expect("counter should parse");
        let bindings = top_level_bindings(&expressions);
        assert!(bindings.contains_key("document"));
        assert!(bindings.contains_key("counter"));
        assert!(bindings.contains_key("increment_button"));
    }

    #[test]
    fn finds_structural_todo_mvc_markers_without_raw_string_search() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let expressions = parse_static_expressions(source).expect("todo_mvc should parse");

        assert!(contains_top_level_function(&expressions, "new_todo"));
        assert!(contains_function_call_path(
            &expressions,
            &["Router", "go_to"]
        ));
        assert!(contains_function_call_path(
            &expressions,
            &["Element", "checkbox"]
        ));
        assert!(contains_alias_path(
            &expressions,
            &[
                "todo",
                "todo_elements",
                "todo_title_element",
                "event",
                "double_click"
            ]
        ));
        assert!(contains_text_fragment(
            &expressions,
            "Double-click to edit a todo"
        ));
    }

    #[test]
    fn finds_control_flow_shapes_without_raw_string_search() {
        let latest = include_str!("../../../playground/frontend/src/examples/latest/latest.bn");
        let latest = parse_static_expressions(latest).expect("latest should parse");
        assert!(contains_latest_expression(&latest));
        assert!(contains_then_expression(&latest));

        let then = include_str!("../../../playground/frontend/src/examples/then/then.bn");
        let then = parse_static_expressions(then).expect("then should parse");
        assert!(contains_hold_expression(&then));
        assert!(contains_then_expression(&then));

        let when = include_str!("../../../playground/frontend/src/examples/when/when.bn");
        let when = parse_static_expressions(when).expect("when should parse");
        assert!(contains_latest_expression(&when));
        assert!(contains_when_expression(&when));

        let while_source = include_str!("../../../playground/frontend/src/examples/while/while.bn");
        let while_source = parse_static_expressions(while_source).expect("while should parse");
        assert!(contains_latest_expression(&while_source));
        assert!(contains_while_expression(&while_source));
    }

    #[test]
    fn finds_list_retain_reactive_shape_without_raw_string_search() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_reactive/list_retain_reactive.bn"
        );
        let expressions = parse_static_expressions(source).expect("list_retain_reactive parses");

        assert!(contains_hold_expression(&expressions));
        assert!(contains_then_expression(&expressions));
        assert!(contains_when_expression(&expressions));
        assert!(contains_alias_path(
            &expressions,
            &["store", "toggle", "event", "press"]
        ));
        assert!(contains_function_call_path(
            &expressions,
            &["List", "retain"]
        ));
    }
}
