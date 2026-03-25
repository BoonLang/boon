use boon::parser::{
    Input as _, Parser as _, SourceCode, StrSlice, Token, lexer, parser, reset_expression_depth,
    resolve_references, span_at, static_expression,
};
use std::collections::BTreeMap;

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

    Ok(static_expression::convert_expressions(source_code, ast))
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
}
